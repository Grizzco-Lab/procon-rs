#!/usr/bin/env bash
# scripts/cleanup_gadget.sh
set -euo pipefail

G=/sys/kernel/config/usb_gadget/procon

echo "Cleaning up existing HID gadget configuration..."

# Check if gadget exists
if [ ! -d "$G" ]; then
    echo "No existing gadget configuration found."
    exit 0
fi

# 1. Unbind from UDC if bound
if [ -f "$G/UDC" ] && [ -s "$G/UDC" ]; then
    echo "Unbinding from UDC..."
    echo "" > "$G/UDC" || true
fi

# 2. Remove symlinks from configs
if [ -d "$G/configs/c.1" ]; then
    echo "Removing function symlinks..."
    find "$G/configs/c.1" -type l -delete || true
fi

# 3. Remove functions
if [ -d "$G/functions" ]; then
    echo "Removing functions..."
    rmdir "$G/functions/"*/ 2>/dev/null || true
    rmdir "$G/functions" 2>/dev/null || true
fi

# 4. Remove configs
if [ -d "$G/configs" ]; then
    echo "Removing configs..."
    rmdir "$G/configs/"*/ 2>/dev/null || true
    rmdir "$G/configs" 2>/dev/null || true
fi

# 5. Remove strings
if [ -d "$G/strings" ]; then
    echo "Removing strings..."
    rmdir "$G/strings/"*/ 2>/dev/null || true
    rmdir "$G/strings" 2>/dev/null || true
fi

# 6. Remove gadget directory
echo "Removing gadget directory..."
rmdir "$G" 2>/dev/null || true

echo "Cleanup completed."
