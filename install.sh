#!/bin/sh
# Builds Sonda and puts it in ~/.local: the program in bin, llama.cpp's
# libraries in lib/sonda, the models and the window in share/sonda.
# `scripts/build-llama.sh` must have built llama.cpp first.
set -eu
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
[ -f vendor/llama-build/bin/libllama.so ] || sh scripts/build-llama.sh
nice -n 19 cargo build --release -j 3
mkdir -p "$PREFIX/bin" "$PREFIX/lib/sonda"
for lib in vendor/llama-build/bin/lib*.so*; do
    cp -a "$lib" "$PREFIX/lib/sonda/"
done
cp target/release/sonda "$PREFIX/bin/sonda.new"
mv "$PREFIX/bin/sonda.new" "$PREFIX/bin/sonda"
# The models, without a second copy when they are here already.
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/sonda/models"
mkdir -p "$DATA"
for m in models/*.gguf; do
    [ -f "$m" ] || continue
    [ -f "$DATA/$(basename "$m")" ] || ln "$m" "$DATA/" 2>/dev/null || cp --reflink=auto "$m" "$DATA/"
done
"$PREFIX/bin/sonda" setup
echo "Sonda is in $PREFIX/bin/sonda"
# The Open/Save dialog of every program: xdg-desktop-portal only reads the
# portals in /usr/share, so this one line needs root. Asked for here, at a
# terminal, only when the file is missing or changed; otherwise said.
PORTAL=/usr/share/xdg-desktop-portal/portals/sonda.portal
if ! cmp -s data/sonda.portal "$PORTAL"; then
    if [ -t 0 ] && command -v sudo > /dev/null 2>&1; then
        echo "For the Open/Save dialog of every program, sudo installs $PORTAL:"
        sudo install -Dm644 data/sonda.portal "$PORTAL" || echo "  not installed; later: sudo install -Dm644 data/sonda.portal $PORTAL"
    else
        echo "For the Open/Save dialog of every program (once, as root):"
        echo "  sudo install -Dm644 data/sonda.portal $PORTAL"
    fi
fi
