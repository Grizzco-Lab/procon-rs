#!/usr/bin/env bash
# scripts/gadget_procon_bidirectional.sh
set -euo pipefail

# First cleanup existing configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"$SCRIPT_DIR/cleanup_gadget.sh"

G=/sys/kernel/config/usb_gadget/procon

echo "Setting up bidirectional HID gadget..."

# Create gadget directory
mkdir -p $G

# Basic gadget configuration
echo 0x057E > $G/idVendor     # Nintendo
echo 0x2009 > $G/idProduct    # Pro Controller (wired)
echo 0x0200 > $G/bcdUSB

# Device strings
mkdir -p $G/strings/0x409
echo "NS Pro Proxy" > $G/strings/0x409/product
echo "0001"         > $G/strings/0x409/serialnumber
echo "Proxy Co."    > $G/strings/0x409/manufacturer

# Configuration
mkdir -p $G/configs/c.1
echo 120 > $G/configs/c.1/MaxPower

# HID function with bidirectional support
mkdir -p $G/functions/hid.usb0
echo 0 > $G/functions/hid.usb0/protocol
echo 0 > $G/functions/hid.usb0/subclass
echo 64 > $G/functions/hid.usb0/report_length

# Bidirectional HID report descriptor
# Supports both input reports (controller -> NS) and output reports (NS -> controller)
cat > $G/functions/hid.usb0/report_desc << 'HEX'
\x05\x01        \x09\x05        \xA1\x01
  \x85\x30      \x15\x00        \x26\xFF\x00
  \x75\x08      \x95\x40        \x09\x01 \x81\x02   # Input report 0x30, 64 bytes
  \x85\x10      \x15\x00        \x26\xFF\x00
  \x75\x08      \x95\x40        \x09\x02 \x91\x02   # Output report 0x10, 64 bytes
\xC0
HEX

# Link function to configuration
ln -s $G/functions/hid.usb0 $G/configs/c.1/

# Bind to UDC
UDC=$(ls /sys/class/udc | head -n1)
if [ -z "$UDC" ]; then
    echo "Error: No UDC found!"
    exit 1
fi

echo $UDC > $G/UDC
echo "[OK] Bidirectional HID gadget up on $UDC"

# Check if device was created
sleep 1
if [ -c /dev/hidg0 ]; then
    echo "[OK] /dev/hidg0 device created"
    ls -l /dev/hidg0
else
    echo "[WARNING] /dev/hidg0 device not found"
fi