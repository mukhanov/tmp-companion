#!/bin/sh
# Post-remove hook for the .deb/.rpm bundle — reloads udev after the rule
# installed alongside it (packaging/udev/70-fender-tone-master-pro.rules) has
# been removed by the package manager, so the uaccess tag stops applying to a
# unit that is still plugged in, with no unplug/replug required.
#
# The mirror image of postinst.sh, and non-fatal for the same reason: a container
# or chroot has no running udev, and that must not fail the package removal.
# POSIX sh — deb and rpm both invoke this with /bin/sh, not bash.
#
# The trigger is NOT redundant with the reload. A reload updates udevd's rule
# set for FUTURE events only; it does not re-evaluate devices that are already
# present, so a unit still plugged in at uninstall keeps the `uaccess` ACL the
# departed rule gave it until something replays its uevent. That is the same
# mechanism postinst.sh relies on in the opposite direction — it triggers so an
# already-connected unit GAINS access without a replug — so removal needs the
# mirror image, or access outlives the package that granted it.
set -e

if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=hidraw || true
fi
