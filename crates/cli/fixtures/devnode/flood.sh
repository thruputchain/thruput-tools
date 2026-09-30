#!/bin/sh
# Writes without bound, so the probe has to enforce its output limit.
while :; do printf '0123456789abcdef0123456789abcdef\n'; done
