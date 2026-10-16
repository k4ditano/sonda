// EmbeddingGemma 2 on llama.cpp: text, pictures and sound into one 768-wide
// space. The image and audio encoders (mtmd) feed the text model, and its
// mean-pooled, L2-normalised output is the vector. Inputs are framed as the
// reference frames them: `<bos> text <eos>`, `<bos> <|image> soft tokens
// <image|> <eos>`, and the same for audio. The model is non-causal with no KV
// cache, so each input —token ids and encoder rows together— goes through one
// `llama_process` call. (The way DigUp's LlamaEmbedder does it, MIT.)
//
// One thread at a time per context.

#include <math.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ggml-backend.h"
#include "llama.h"
#include "mtmd.h"

#define MAX_SEQUENCES 16

typedef struct {
    struct llama_model *model;
    struct llama_context *ctx;
    mtmd_context *media;
    struct llama_batch_ext *batch;
    const struct llama_vocab *vocab;
    int dim;        // 768: the output projection
    int row;        // the width of encoder rows fed to the text model
    int max_tokens;
    int sequences;  // in `batch`
    int tokens;     // in `batch`
    llama_pos position;
} sonda_embedder;

static char last_error[512];
static int quiet = 1;

static void fail(const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vsnprintf(last_error, sizeof last_error, fmt, args);
    va_end(args);
}

const char *sonda_error(void) { return last_error; }

// llama.cpp says a lot while it loads: only its warnings and errors.
static void on_log(enum ggml_log_level level, const char *text, void *data) {
    (void)data;
    static enum ggml_log_level was = GGML_LOG_LEVEL_INFO;
    if (level != GGML_LOG_LEVEL_CONT) was = level;
    if (quiet && was < GGML_LOG_LEVEL_WARN) return;
    fputs(text, stderr);
}

static void reset(sonda_embedder *e) {
    llama_batch_ext_clear(e->batch);
    e->sequences = 0;
    e->tokens = 0;
}

static void begin(sonda_embedder *e) {
    e->sequences += 1;
    e->position = 0;
}

static int place(sonda_embedder *e, int32_t index) {
    if (index < 0) {
        fail("the batch is full (%d)", index);
        return -1;
    }
    llama_pos pos[4] = {e->position, 0, 0, 0};
    if (!llama_batch_ext_set_pos(e->batch, index, pos) || !llama_batch_ext_set_output_embd(e->batch, index, true)) {
        fail("cannot place token %d", index);
        return -1;
    }
    e->position += 1;
    e->tokens += 1;
    return 0;
}

static int add_token(sonda_embedder *e, llama_token t) {
    return place(e, llama_batch_ext_add_token(e->batch, e->sequences - 1, t));
}

static int add_rows(sonda_embedder *e, const float *rows, size_t count) {
    for (size_t r = 0; r < count; r++) {
        struct llama_embd embd = {rows + r * e->row, 1, (size_t)e->row};
        if (place(e, llama_batch_ext_add_embd(e->batch, e->sequences - 1, embd)) != 0) return -1;
    }
    return 0;
}

// Runs the text model over the batch: one normalised vector per input, in order.
static int run(sonda_embedder *e, float *out) {
    int status = llama_process(e->ctx, LLAMA_PROCESS_TYPE_ENCODE, e->batch);
    if (status != 0) {
        fail("llama_process failed (%d)", status);
        reset(e);
        return -1;
    }
    for (int s = 0; s < e->sequences; s++) {
        const float *pooled = llama_get_embeddings_seq(e->ctx, s);
        if (!pooled) {
            fail("no pooled vector for input %d", s);
            reset(e);
            return -1;
        }
        double sum = 0;
        for (int i = 0; i < e->dim; i++) sum += (double)pooled[i] * pooled[i];
        float scale = sum > 0 ? (float)(1.0 / sqrt(sum)) : 0;
        for (int i = 0; i < e->dim; i++) out[s * e->dim + i] = pooled[i] * scale;
    }
    reset(e);
    return 0;
}

static void start_backends(void) {
    ggml_backend_load_all();
    llama_backend_init();
}

void *sonda_open(const char *text_model, const char *mmproj, int gpu, int threads, int verbose) {
    quiet = !verbose;
    llama_log_set(on_log, NULL);
    mtmd_log_set(on_log, NULL);
    // Once per process, whichever thread comes first (the reader and a
    // search may open their models at the same moment).
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, start_backends);
    struct llama_model_params mp = llama_model_default_params();
    mp.n_gpu_layers = gpu ? 999 : 0;
    struct llama_model *model = llama_model_load_from_file(text_model, mp);
    if (!model) {
        fail("cannot load %s", text_model);
        return NULL;
    }
    // One input must fit one micro-batch: 2048 tokens, which also holds a
    // 1120-token picture or a 750-token half minute of sound. A query encoder
    // reserves less.
    int max_tokens = mmproj ? 2048 : 512;
    struct llama_context_params cp = llama_context_default_params();
    cp.n_ctx = max_tokens;
    cp.n_batch = max_tokens;
    cp.n_ubatch = max_tokens;
    cp.n_seq_max = MAX_SEQUENCES;
    cp.kv_unified = true;
    cp.embeddings = true;
    cp.pooling_type = LLAMA_POOLING_TYPE_MEAN;
    if (threads > 0) {
        cp.n_threads = threads;
        cp.n_threads_batch = threads;
    }
    struct llama_context *ctx = llama_init_from_model(model, cp);
    if (!ctx) {
        llama_model_free(model);
        fail("cannot make a llama.cpp context");
        return NULL;
    }
    mtmd_context *media = NULL;
    if (mmproj) {
        struct mtmd_context_params p = mtmd_context_params_default();
        p.use_gpu = gpu != 0;
        p.print_timings = false;
        p.warmup = true;
        p.image_min_tokens = 1;     // never rescaled here: pictures come at their final size
        p.image_max_tokens = 1120;
        if (threads > 0) p.n_threads = threads;
        media = mtmd_init_from_file(mmproj, model, p);
        if (!media) {
            llama_free(ctx);
            llama_model_free(model);
            fail("cannot load %s", mmproj);
            return NULL;
        }
    }
    sonda_embedder *e = calloc(1, sizeof *e);
    e->model = model;
    e->ctx = ctx;
    e->media = media;
    e->batch = llama_batch_ext_init(ctx);
    e->vocab = llama_model_get_vocab(model);
    e->dim = llama_model_n_embd_out(model);
    e->row = llama_model_n_embd_inp(model);
    e->max_tokens = max_tokens;
    return e;
}

int sonda_dim(void *handle) { return ((sonda_embedder *)handle)->dim; }

int sonda_sample_rate(void *handle) {
    sonda_embedder *e = handle;
    return e->media ? mtmd_get_audio_sample_rate(e->media) : 16000;
}

void sonda_close(void *handle) {
    sonda_embedder *e = handle;
    if (!e) return;
    if (e->batch) llama_batch_ext_free(e->batch);
    if (e->media) mtmd_free(e->media);
    if (e->ctx) llama_free(e->ctx);
    if (e->model) llama_model_free(e->model);
    free(e);
}

// `<bos> text <eos>`, cut to `max_tokens` the way transformers cuts (keeping
// both special tokens). Returns how many.
static int tokenize(sonda_embedder *e, const char *text, llama_token **out) {
    int len = (int)strlen(text);
    int cap = len + 8;
    llama_token *t = malloc(sizeof *t * cap);
    int n = llama_tokenize(e->vocab, text, len, t, cap, true, true);
    if (n < 0) {
        cap = -n;
        t = realloc(t, sizeof *t * cap);
        n = llama_tokenize(e->vocab, text, len, t, cap, true, true);
    }
    if (n < 0) n = 0;
    if (n > e->max_tokens) {
        n = e->max_tokens;
        t[n - 1] = llama_vocab_eos(e->vocab);
    }
    *out = t;
    return n;
}

// Several texts at once, `out` holding `count * dim` floats.
int sonda_embed_texts(void *handle, const char **texts, int count, float *out) {
    sonda_embedder *e = handle;
    reset(e);
    int done = 0;
    for (int i = 0; i < count; i++) {
        llama_token *t;
        int n = tokenize(e, texts[i], &t);
        if (e->sequences == MAX_SEQUENCES || e->tokens + n > e->max_tokens) {
            int was = e->sequences;
            if (run(e, out + (size_t)done * e->dim) != 0) {
                free(t);
                return -1;
            }
            done += was;
        }
        begin(e);
        for (int k = 0; k < n; k++) {
            if (add_token(e, t[k]) != 0) {
                free(t);
                reset(e);
                return -1;
            }
        }
        free(t);
    }
    if (e->sequences > 0) {
        int was = e->sequences;
        if (run(e, out + (size_t)done * e->dim) != 0) return -1;
        done += was;
    }
    return done == count ? 0 : -1;
}

static int embed_bitmap(sonda_embedder *e, mtmd_bitmap *bitmap, float *out) {
    mtmd_input_chunks *chunks = mtmd_input_chunks_init();
    const char *marker = mtmd_default_marker();
    mtmd_input_text text = {marker, strlen(marker), true, true};
    const mtmd_bitmap *bitmaps[1] = {bitmap};
    int status = mtmd_tokenize(e->media, chunks, &text, bitmaps, 1);
    if (status != 0) {
        fail("cannot tokenize the input (%d)", status);
        mtmd_input_chunks_free(chunks);
        return -1;
    }
    reset(e);
    begin(e);
    size_t n = mtmd_input_chunks_size(chunks);
    for (size_t i = 0; i < n; i++) {
        const mtmd_input_chunk *chunk = mtmd_input_chunks_get(chunks, i);
        if (mtmd_input_chunk_get_type(chunk) == MTMD_INPUT_CHUNK_TYPE_TEXT) {
            size_t count = 0;
            const llama_token *tokens = mtmd_input_chunk_get_tokens_text(chunk, &count);
            for (size_t k = 0; k < count; k++) {
                if (add_token(e, tokens[k]) != 0) goto failed;
            }
        } else {
            if (mtmd_encode_chunk(e->media, chunk) != 0) {
                fail("the encoder failed");
                goto failed;
            }
            const float *rows = mtmd_get_output_embd(e->media);
            if (!rows || add_rows(e, rows, mtmd_input_chunk_get_n_tokens(chunk)) != 0) goto failed;
        }
    }
    mtmd_input_chunks_free(chunks);
    return run(e, out);
failed:
    mtmd_input_chunks_free(chunks);
    reset(e);
    return -1;
}

// A picture already sized for its token budget, packed RGB.
int sonda_embed_rgb(void *handle, int width, int height, const unsigned char *rgb, float *out) {
    sonda_embedder *e = handle;
    if (!e->media) {
        fail("this embedder was opened for text only");
        return -1;
    }
    mtmd_bitmap *bitmap = mtmd_bitmap_init((uint32_t)width, (uint32_t)height, rgb);
    if (!bitmap) {
        fail("cannot make a bitmap of %dx%d", width, height);
        return -1;
    }
    int status = embed_bitmap(e, bitmap, out);
    mtmd_bitmap_free(bitmap);
    return status;
}

// Sound, mono at the encoder's rate (16 kHz), 30 s at most.
int sonda_embed_pcm(void *handle, const float *samples, size_t count, float *out) {
    sonda_embedder *e = handle;
    if (!e->media) {
        fail("this embedder was opened for text only");
        return -1;
    }
    size_t limit = 30 * (size_t)mtmd_get_audio_sample_rate(e->media);
    if (count > limit) count = limit;
    mtmd_bitmap *bitmap = mtmd_bitmap_init_from_audio(count, samples);
    if (!bitmap) {
        fail("cannot make an audio bitmap");
        return -1;
    }
    int status = embed_bitmap(e, bitmap, out);
    mtmd_bitmap_free(bitmap);
    return status;
}

int sonda_has_gpu(void) {
    for (size_t i = 0; i < ggml_backend_dev_count(); i++) {
        enum ggml_backend_dev_type t = ggml_backend_dev_type(ggml_backend_dev_get(i));
        if (t == GGML_BACKEND_DEVICE_TYPE_GPU || t == GGML_BACKEND_DEVICE_TYPE_IGPU) return 1;
    }
    return 0;
}
