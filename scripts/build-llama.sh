#!/bin/sh
# llama.cpp at the tag EmbeddingGemma 2 came with (text, pictures and sound),
# with DigUp's one-line patch (an embedding context reserves no logits), built
# with Vulkan where there is a card and its tools, into vendor/llama-build.
set -eu
cd "$(dirname "$0")/.."
TAG=b11461
COMMIT=4d756bc72bf00a4aacf410ae15a2d315f3db400d
mkdir -p vendor
if [ ! -d vendor/llama.cpp ]; then
    git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$TAG" https://github.com/ggml-org/llama.cpp vendor/llama.cpp
fi
[ "$(git -C vendor/llama.cpp rev-parse HEAD)" = "$COMMIT" ] || { echo "llama.cpp is not at $TAG" >&2; exit 1; }
if git -C vendor/llama.cpp apply --check "$PWD/scripts/llama.cpp-no-logits.patch" 2>/dev/null; then
    git -C vendor/llama.cpp apply "$PWD/scripts/llama.cpp-no-logits.patch"
fi
# Vulkan's and SPIR-V's headers, when the system has not got them.
VULKAN=OFF
if command -v glslc >/dev/null; then
    VULKAN=ON
    if [ ! -f /usr/include/vulkan/vulkan.h ]; then
        for r in Vulkan-Headers SPIRV-Headers; do
            [ -d "vendor/$r" ] || git clone --quiet --depth 1 "https://github.com/KhronosGroup/$r" "vendor/$r"
            cmake -S "vendor/$r" -B "vendor/build-$r" -DCMAKE_INSTALL_PREFIX="$PWD/vendor/prefix" >/dev/null
            cmake --install "vendor/build-$r" >/dev/null
        done
    fi
fi
nice -n 19 cmake -S vendor/llama.cpp -B vendor/llama-build -DCMAKE_PREFIX_PATH="$PWD/vendor/prefix" -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=ON -DGGML_VULKAN=$VULKAN -DGGML_NATIVE=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
    -DLLAMA_BUILD_SERVER=OFF -DLLAMA_BUILD_TOOLS=ON -DLLAMA_BUILD_MTMD=ON -DLLAMA_OPENSSL=OFF -DLLAMA_CURL=OFF \
    -DLLAMA_BUILD_APP=OFF -DLLAMA_BUILD_UI=OFF >/dev/null
nice -n 19 ionice -c3 cmake --build vendor/llama-build --config Release -j 3
