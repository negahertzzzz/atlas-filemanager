#!/usr/bin/env bash
# Executed as root inside a disposable distribution container.
set -euo pipefail
format=${1:?Specify deb, rpm, or archlinux}
dist=${2:-/artifacts}
case "$format" in
  deb)
    # Minimal Ubuntu images discard documentation by default. Keep Atlas's
    # notices so this test exercises the payload installed on a desktop.
    printf 'path-include=/usr/share/doc/atlas\npath-include=/usr/share/doc/atlas/*\n' \
      > /etc/dpkg/dpkg.cfg.d/zz-atlas-docs
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y "$dist"/*.deb \
      xvfb xauth xdotool imagemagick mesa-vulkan-drivers libegl1 libgl1-mesa-dri fonts-dejavu-core dbus-x11 python3
    dpkg-query -W atlas
    desktop-file-validate /usr/share/applications/atlas.desktop 2>/dev/null || {
      apt-get install -y desktop-file-utils
      desktop-file-validate /usr/share/applications/atlas.desktop
    } ;;
  rpm)
    if command -v dnf >/dev/null; then
      dnf install -y "$dist"/*.rpm xorg-x11-server-Xvfb xorg-x11-xauth xdotool ImageMagick mesa-vulkan-drivers mesa-libEGL mesa-dri-drivers dejavu-sans-fonts dbus-x11 desktop-file-utils python3
    else
      zypper --non-interactive --gpg-auto-import-keys refresh
      zypper --non-interactive install --allow-unsigned-rpm "$dist"/*.rpm \
        xorg-x11-server-Xvfb xvfb-run xauth xdotool ImageMagick libvulkan_lvp Mesa-libEGL1 Mesa-dri dejavu-fonts dbus-1-daemon desktop-file-utils python3
    fi
    rpm -q atlas
    desktop-file-validate /usr/share/applications/atlas.desktop ;;
  archlinux)
    # Override the equivalent size-saving filter in the Arch container.
    sed -i '/^[[:space:]]*NoExtract[[:space:]]*=/s|$| !usr/share/doc/atlas !usr/share/doc/atlas/*|' /etc/pacman.conf
    pacman -Syu --noconfirm
    pacman -S --noconfirm --needed xorg-server-xvfb xorg-xauth xdotool imagemagick vulkan-swrast mesa libglvnd ttf-dejavu dbus desktop-file-utils diffutils python
    pacman -U --noconfirm "$dist"/*.pkg.tar.zst
    pacman -Q atlas
    desktop-file-validate /usr/share/applications/atlas.desktop ;;
  *) echo 'Unknown package format' >&2; exit 2 ;;
esac
for binary in atlas-search-service atlas-cloud-extension atlas-icon-extension; do
  test -x "/usr/bin/$binary"
  ldd "/usr/bin/$binary" | tee "/logs/$binary-ldd.txt"
  if grep -q 'not found' "/logs/$binary-ldd.txt"; then exit 1; fi
done
test -s /usr/share/doc/atlas/THIRD-PARTY-NOTICES.txt
ldd /usr/bin/atlas | tee /logs/ldd.txt
if grep -q 'not found' /logs/ldd.txt; then exit 1; fi
if ! id atlas-test >/dev/null 2>&1; then useradd --create-home atlas-test; fi
if [[ $format == archlinux ]]; then
  mkdir -p /tmp/atlas-aur
  tar -xzf "$dist"/atlas-*-aur.tar.gz -C /tmp/atlas-aur
  chown -R atlas-test:atlas-test /tmp/atlas-aur
  runuser -u atlas-test -- bash -c 'cd /tmp/atlas-aur/atlas-bin && makepkg --printsrcinfo' > /tmp/atlas-srcinfo
  diff -u <(sed 's/^[[:space:]]*//' /tmp/atlas-aur/atlas-bin/.SRCINFO | sort) \
    <(sed 's/^[[:space:]]*//' /tmp/atlas-srcinfo | sort)
fi
chmod -R 777 /logs
runuser -u atlas-test -- python3 /scripts/test-linux-search-service.py /usr/bin/atlas-search-service | tee /logs/search-service.txt
runuser -u atlas-test -- env ATLAS_VERSION="$ATLAS_VERSION" ATLAS_SMOKE_LOG_DIR=/logs \
  dbus-run-session -- xvfb-run -a -s '-screen 0 1280x900x24' bash /scripts/linux-smoke.sh /usr/bin/atlas
chmod -R a+rwX /logs
