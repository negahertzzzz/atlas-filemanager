#!/usr/bin/env bash
# Local Flatpak/Snap artifacts for testing. This does not submit anything to a store.
set -euo pipefail
format=${1:?Specify flatpak or snap}
dist=${2:?Specify the Linux artifact directory}
version=${ATLAS_VERSION:?Set ATLAS_VERSION}
app_id=io.github.neondeex.atlas-filemanager
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
tar -xzf "$dist/atlas-$version-linux-x86_64.tar.gz" -C "$scratch"
payload=$scratch/atlas-$version-linux-x86_64
case "$format" in
  flatpak)
    runtime=25.08
    flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
    flatpak install --user --noninteractive flathub "org.freedesktop.Platform//$runtime" "org.freedesktop.Sdk//$runtime"
    flatpak build-init "$scratch/build" "$app_id" org.freedesktop.Sdk org.freedesktop.Platform "$runtime"
    cp -a "$payload/bin" "$payload/share" "$scratch/build/files/"
    mv "$scratch/build/files/share/applications/atlas.desktop" "$scratch/build/files/share/applications/$app_id.desktop"
    sed -i "s/^Icon=atlas$/Icon=$app_id/" "$scratch/build/files/share/applications/$app_id.desktop"
    mv "$scratch/build/files/share/icons/hicolor/512x512/apps/atlas.png" "$scratch/build/files/share/icons/hicolor/512x512/apps/$app_id.png"
    sed -i "s/>atlas.desktop</>$app_id.desktop</" "$scratch/build/files/share/metainfo/$app_id.metainfo.xml"
    flatpak build-finish --command=atlas --share=ipc --share=network --socket=x11 --socket=wayland \
      --device=dri --filesystem=host "$scratch/build"
    flatpak build-export "$scratch/repo" "$scratch/build"
    flatpak build-bundle "$scratch/repo" "$dist/Atlas-$version-x86_64.flatpak" "$app_id" \
      --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
    flatpak install --user --noninteractive "$dist/Atlas-$version-x86_64.flatpak"
    flatpak run "$app_id" --version
    ;;
  snap)
    # core22 matches the Ubuntu 22.04 ELF baseline. Classic needs store approval.
    mkdir -p "$scratch/snap/meta/gui" "$scratch/snap/usr"
    cp -a "$payload/bin" "$payload/share" "$scratch/snap/usr/"
    cp "$payload/share/applications/atlas.desktop" "$scratch/snap/meta/gui/atlas.desktop"
    cp "$payload/share/icons/hicolor/512x512/apps/atlas.png" "$scratch/snap/meta/gui/atlas.png"
    for desktop in "$scratch/snap/meta/gui/atlas.desktop" "$scratch/snap/usr/share/applications/atlas.desktop"; do
      sed -i "s|^Icon=atlas$|Icon=\${SNAP}/meta/gui/atlas.png|" "$desktop"
    done
    # The bundle collects non-base libraries that classic confinement must locate.
    while IFS= read -r library; do
      case "$library" in
        */libc.so.*|*/libm.so.*|*/libpthread.so.*|*/libdl.so.*|*/ld-linux*|*/libgcc_s.so.*) continue ;;
      esac
      [[ -f $library ]] || continue
      mkdir -p "$scratch/snap/usr/lib/atlas"
      cp -L "$library" "$scratch/snap/usr/lib/atlas/"
      owner=$(dpkg-query -S "$library" "$(readlink -f "$library")" 2>/dev/null | head -n1 | cut -d: -f1 || true)
      if [[ -n $owner && -f /usr/share/doc/$owner/copyright ]]; then
        mkdir -p "$scratch/snap/usr/share/doc/$owner"
        cp "/usr/share/doc/$owner/copyright" "$scratch/snap/usr/share/doc/$owner/"
      fi
    done < <(for binary in "$payload/bin/"*; do ldd "$binary"; done | awk '$2 == "=>" && $3 ~ /^\// {print $3}' | sort -u)
    cp -a /usr/share/common-licenses "$scratch/snap/usr/share/"
    cat > "$scratch/snap/meta/snap.yaml" <<EOF
name: atlas-filemanager
version: '$version'
summary: Modern native file manager
description: Manage files with multiple panes, previews and keyboard navigation.
base: core22
grade: stable
confinement: classic
architectures: [amd64]
apps:
  atlas:
    command: usr/bin/atlas
    desktop: usr/share/applications/atlas.desktop
    environment:
      LD_LIBRARY_PATH: \$SNAP/usr/lib/atlas
EOF
    snap pack --check-skeleton "$scratch/snap"
    snap pack --compression=xz --filename="Atlas-$version-x86_64.snap" "$scratch/snap" "$dist"
    ;;
  *) echo 'Unknown store package format' >&2; exit 2 ;;
esac
