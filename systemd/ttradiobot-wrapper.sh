#!/bin/sh
# Use the headless ALSA configuration for native SDK device discovery.
export ALSA_CONFIG_PATH="${ALSA_CONFIG_PATH:-$HOME/.config/ttradiobot/alsa.conf}"
exec "$HOME/.local/bin/ttradiobot.bin" "$@"
