# Build from the root of a DBDeck source checkout.
pkgname=zer0-dbdeck
pkgver=0.1.0
pkgrel=1
pkgdesc='Native Wayland database client for PostgreSQL and MySQL/MariaDB'
arch=('x86_64')
url='https://github.com/andrey-losikhin/zer0-dbdeck'
license=('Apache-2.0')
depends=('adwaita-icon-theme' 'cairo' 'glib2' 'glibc' 'graphene' 'gtk4'
         'gtksourceview5' 'hicolor-icon-theme' 'libadwaita' 'libgcc' 'pango'
         'webkitgtk-6.0')
makedepends=('pkgconf' 'rust')
# GCC LTO objects from ring cannot be linked by Rust's LLVM linker.
options=('!lto')
optdepends=('libsecret: desktop keyring auto-unlock (requires a Secret Service provider)'
            'openssh: SSH tunnel connections'
            'gopass: password store integration and auto-unlock'
            'postgresql-libs: PostgreSQL backup and restore tools'
            'mariadb-clients: MariaDB backup and restore tools')

prepare() {
    cd "$startdir"
    cargo fetch --locked --target "$CARCH-unknown-linux-gnu"
}

build() {
    cd "$startdir"
    cargo build --release --frozen --target "$CARCH-unknown-linux-gnu" --target-dir "$srcdir/target"
}

package() {
    cd "$startdir"
    local app_id=io.github.andrey-losikhin.zer0-dbdeck
    local licenses="$pkgdir/usr/share/licenses/$pkgname"

    install -Dm755 "$srcdir/target/$CARCH-unknown-linux-gnu/release/$pkgname" "$pkgdir/usr/bin/$pkgname"
    install -Dm644 "packaging/arch/$app_id.desktop" "$pkgdir/usr/share/applications/$app_id.desktop"
    install -Dm644 "packaging/arch/$app_id.svg" "$pkgdir/usr/share/icons/hicolor/scalable/apps/$app_id.svg"

    install -d "$licenses"
    cp --parents LICENSE NOTICE docs/THIRD_PARTY_NOTICES.md docs/licenses/*.txt \
        assets/icons/vendors/LICENSE-MDI.txt assets/icons/vendors/LICENSE-Simple-Icons.md \
        assets/vendor/tabulator-6.5.3/LICENSE assets/vendor/tabulator-6.6.1/LICENSE \
        "$licenses/"
}
