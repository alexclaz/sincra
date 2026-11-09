#!/bin/sh
# Sincra installer for macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/alexclaz/sincra/main/install.sh | sh
#
# Installs "Sincra.app" in ~/Applications (GUI, CLI and ffmpeg inside) and links the
# `sincra` command into ~/.local/bin. Files fetched with curl carry no quarantine flag, so
# Gatekeeper does not block the app even though it is not notarized.
#
# Environment variables:
#   SINCRA_VERSION=v0.2.0   install a specific release instead of the latest
#   SINCRA_UNINSTALL=1      remove the app and the command
set -eu

REPO="${SINCRA_REPO:-alexclaz/sincra}"
APP_NAME="Sincra.app"
APP_DIR="$HOME/Applications"
BIN_DIR="$HOME/.local/bin"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf '\033[31merro:\033[0m %s\n' "$*" >&2; exit 1; }

if [ "${SINCRA_UNINSTALL:-}" = "1" ]; then
    rm -rf "$APP_DIR/$APP_NAME" "$BIN_DIR/sincra"
    say "Sincra removido. (cache de audio: ~/Library/Caches/sincra)"
    exit 0
fi

[ "$(uname -s)" = "Darwin" ] || die "este instalador e para macOS. No Windows (PowerShell): irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex"

case "$(uname -m)" in
    arm64 | aarch64) ARCH=arm64 ;;
    x86_64)
        # a shell running under Rosetta reports x86_64 on Apple Silicon
        if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then ARCH=arm64; else ARCH=x64; fi ;;
    *) die "arquitetura nao suportada: $(uname -m)" ;;
esac

ASSET="sincra-macos-$ARCH.tar.gz"
if [ -n "${SINCRA_VERSION:-}" ]; then
    URL="https://github.com/$REPO/releases/download/$SINCRA_VERSION/$ASSET"
else
    URL="https://github.com/$REPO/releases/latest/download/$ASSET"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ -n "${SINCRA_PACKAGE:-}" ]; then  # local tarball (testing)
    cp "$SINCRA_PACKAGE" "$TMP/$ASSET"
else
    say "Baixando $ASSET..."
    curl -fL --progress-bar "$URL" -o "$TMP/$ASSET" || die "falha ao baixar $URL"
fi
tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -d "$TMP/$APP_NAME" ] || die "pacote invalido"

mkdir -p "$APP_DIR" "$BIN_DIR"
rm -rf "$APP_DIR/$APP_NAME"
mv "$TMP/$APP_NAME" "$APP_DIR/"
xattr -dr com.apple.quarantine "$APP_DIR/$APP_NAME" 2>/dev/null || true
ln -sf "$APP_DIR/$APP_NAME/Contents/MacOS/sincra" "$BIN_DIR/sincra"

say "Instalado em $APP_DIR/$APP_NAME"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        RC="$HOME/.zshrc"
        case "${SHELL:-}" in */bash) RC="$HOME/.bash_profile" ;; esac
        if ! grep -qs '.local/bin' "$RC"; then
            printf '\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$RC"
            say "Adicionei ~/.local/bin ao PATH em $RC (abra um terminal novo)."
        fi ;;
esac

echo
echo "  Interface:        abra 'Sincra' pelo Spotlight/Launchpad"
echo "  Linha de comando: sincra \"Timeline 1.xml\""
echo "  Desinstalar:      curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | SINCRA_UNINSTALL=1 sh"
