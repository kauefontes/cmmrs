# AUR packaging (not yet published)

`PKGBUILD` + `cmmrs.install` + `.SRCINFO` for an AUR `cmmrs` package,
built from the crates.io source tarball. Verified locally with `makepkg`
(build, `cargo test` inside `check()`, and packaging all succeed) —
what's here isn't a draft, it's known-good as of the version it targets.

**Not published to the AUR.** That's a deliberate hold given the state of
AUR account/package trust after the
[2026 malicious-packages incident](https://archlinux.org/news/active-aur-malicious-packages-incident/),
not a technical blocker — nothing here needs to change to publish later,
just the human steps below.

## To actually publish, whenever that happens

1. Create an AUR account at [aur.archlinux.org](https://aur.archlinux.org)
   and add an SSH public key to it.
2. `git clone ssh://aur@aur.archlinux.org/cmmrs.git` (an empty repo — AUR
   creates it lazily on first push of a package by that name).
3. Copy `PKGBUILD` and `cmmrs.install` from this directory into that
   clone. Regenerate `.SRCINFO` rather than copying it verbatim — it must
   be produced by `makepkg --printsrcinfo > .SRCINFO` immediately before
   each push, or it'll drift from the `PKGBUILD` it's supposed to mirror.
4. `makepkg -si` in that clone to build and install for real, as a final
   check.
5. Commit and push `PKGBUILD`, `cmmrs.install`, `.SRCINFO`.

## Keeping it current

`pkgver`/`sha256sums` need bumping on every new `cmmrs` release — get the
new version's checksum with:

```bash
curl -sL https://static.crates.io/crates/cmmrs/cmmrs-<version>.crate | sha256sum
```

`pkgrel` resets to `1` on a `pkgver` bump, and only increments on its own
for a packaging-only fix (no source change). Regenerate `.SRCINFO` again
before pushing, every time.
