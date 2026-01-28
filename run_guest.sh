#!/bin/sh
# Minimal init script for Neurovisor guest VM - VSOCK TEST

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

echo "========================================"
echo "  Neurovisor Guest VM - VSOCK TEST"
echo "========================================"

# Recreate vsock device with correct minor number
rm -f /dev/vsock
mknod /dev/vsock c 10 122
chmod 666 /dev/vsock
echo "[DEBUG] /dev/vsock: $(ls -la /dev/vsock)"

# Wait for host
echo "[GUEST] Waiting 2 seconds for host..."
sleep 2

# Run the minimal vsock test first
echo ""
echo "[GUEST] Running minimal vsock_test (raw libc)..."
/usr/local/bin/vsock_test 2>&1
VSOCK_EXIT=$?
echo "[GUEST] vsock_test exited with code: $VSOCK_EXIT"

# If vsock_test succeeded, try guest_client
if [ $VSOCK_EXIT -eq 0 ]; then
    echo ""
    echo "[GUEST] vsock_test passed! Now trying guest_client..."
    /usr/local/bin/guest_client "Hello from guest" 2>&1
    echo "[GUEST] guest_client exited with code: $?"
fi

echo ""
echo "[GUEST] Shutting down VM..."
sync
poweroff -f
