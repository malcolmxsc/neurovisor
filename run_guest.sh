#!/bin/sh
# Init script for Neurovisor guest VM - Execution Server

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

echo "========================================"
echo "  Neurovisor Guest VM - Execution Server"
echo "========================================"

# Create vsock device with correct minor number
rm -f /dev/vsock
mknod /dev/vsock c 10 122
chmod 666 /dev/vsock

# Start the guest execution server
echo "[GUEST] Starting execution server..."
exec /usr/local/bin/guest_agent
