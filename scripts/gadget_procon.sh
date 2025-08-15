#!/usr/bin/env bash
set -euo pipefail

G=/sys/kernel/config/usb_gadget/procon
mkdir -p $G
echo 0x057E > $G/idVendor     # Nintendo
echo 0x2009 > $G/idProduct    # Pro Controller (wired)
echo 0x0200 > $G/bcdUSB

mkdir -p $G/strings/0x409
echo "NS Pro Proxy" > $G/strings/0x409/product
echo "0001"         > $G/strings/0x409/serialnumber
echo "Proxy Co."    > $G/strings/0x409/manufacturer

mkdir -p $G/configs/c.1
echo 120 > $G/configs/c.1/MaxPower

# --- HID function
mkdir -p $G/functions/hid.usb0
echo 0 > $G/functions/hid.usb0/protocol
echo 0 > $G/functions/hid.usb0/subclass
echo 64 > $G/functions/hid.usb0/report_length

# 报告描述符（简化：游戏手柄 + 自定义 0x30/64B 输入报告窗口，用来“生搬硬转”原始包）
# 说明：这段把常规按钮/轴放进标准 Gamepad，用一个 64B feature/input window 映射原始 0x30 包（便于原样转发）。
cat > $G/functions/hid.usb0/report_desc <<'HEX'
\x05\x01        \x09\x05        \xA1\x01
  \x85\x30      \x15\x00        \x26\xFF\x00
  \x75\x08      \x95\x40        \x09\x01 \x81\x02   # Input report 0x30, 64 bytes raw window
\xC0
HEX

ln -s $G/functions/hid.usb0 $G/configs/c.1/

# Bind to UDC
UDC=$(ls /sys/class/udc | head -n1)
echo $UDC > $G/UDC
echo "[OK] HID gadget up on $UDC"
