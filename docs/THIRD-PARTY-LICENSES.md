# Third-party licences

temur ships as a single static binary that links its whole dependency
tree in. This file enumerates that tree so the licences travelling
inside the artifact are visible without a build. temur's own licence is
MIT; see `LICENSE`.

Scope: the crates that reach the shipped binary, i.e. the normal and
build dependencies resolved for `i686-unknown-linux-musl` with default
features. Dev-dependencies are excluded because no test crate is linked
into a release build.

Regenerate the census with:

```
cargo metadata --format-version 1 \
  --filter-platform i686-unknown-linux-musl \
  | python3 -c 'import json,sys;m=json.load(sys.stdin);pk={p["id"]:p for p in m["packages"]};byid={n["id"]:n for n in m["resolve"]["nodes"]};r=m["resolve"]["root"];s=set();st=[r]
while st:
    i=st.pop()
    if i in s: continue
    s.add(i)
    for d in byid[i]["deps"]:
        if {(k.get("kind") or "normal") for k in d["dep_kinds"]} <= {"dev"}: continue
        st.append(d["pkg"])
s.discard(r)
[print(pk[i]["name"],pk[i]["version"],pk[i].get("license")) for i in sorted(s,key=lambda i:pk[i]["name"])]'
```

## The rule

Permissive only. Every expression below resolves to a permissive
licence: MIT, Apache-2.0, BSD-3-Clause, ISC, Zlib, 0BSD, Unlicense,
BSL-1.0, CDLA-Permissive-2.0, Unicode-3.0, and Apache-2.0 with the LLVM
exception. There is no GPL, LGPL, AGPL, MPL or SSPL crate in the tree,
and none may be added: a copyleft dependency in a statically linked
binary is a licensing change to the product, not an implementation
detail.

This is a documented rule, not an automated gate. `scripts/check.sh`
enforces only the two forbidden native dependencies (`openssl-sys`,
`aws-lc-sys`); the licence census is a human check run when a
dependency is added.

## Census

As of 2026-09-08, at T54 (rides v0.34.0): **177 crates**, all permissive.

### By licence expression

| licence expression | crates |
|---|---|
| MIT OR Apache-2.0 | 103 |
| MIT | 29 |
| MIT/Apache-2.0 | 8 |
| Apache-2.0 OR MIT | 7 |
| Unlicense OR MIT | 6 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 2 |
| Apache-2.0/MIT | 2 |
| ISC | 2 |
| MIT / Apache-2.0 | 2 |
| Unlicense/MIT | 2 |
| Zlib | 2 |
| (Apache-2.0 OR MIT) AND BSD-3-Clause | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| 0BSD OR MIT OR Apache-2.0 | 1 |
| Apache-2.0 | 1 |
| Apache-2.0 AND ISC | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| Apache-2.0 OR ISC OR MIT | 1 |
| BSD-3-Clause | 1 |
| CDLA-Permissive-2.0 | 1 |
| MIT OR Apache-2.0 OR Zlib | 1 |
| MIT OR Zlib OR Apache-2.0 | 1 |
| Zlib OR Apache-2.0 OR MIT | 1 |

### Every crate

| crate | version | licence |
|---|---|---|
| `adler2` | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| `adobe-cmap-parser` | 0.4.1 | MIT |
| `aes` | 0.8.4 | MIT OR Apache-2.0 |
| `aho-corasick` | 1.1.4 | Unlicense OR MIT |
| `allocator-api2` | 0.2.21 | MIT OR Apache-2.0 |
| `anstream` | 1.0.0 | MIT OR Apache-2.0 |
| `anstyle` | 1.0.14 | MIT OR Apache-2.0 |
| `anstyle-parse` | 1.0.0 | MIT OR Apache-2.0 |
| `anstyle-query` | 1.1.5 | MIT OR Apache-2.0 |
| `atoi_simd` | 0.18.1 | MIT OR Apache-2.0 |
| `autocfg` | 1.5.1 | Apache-2.0 OR MIT |
| `base64` | 0.22.1 | MIT OR Apache-2.0 |
| `bitflags` | 1.3.2 | MIT/Apache-2.0 |
| `bitflags` | 2.13.0 | MIT OR Apache-2.0 |
| `block-buffer` | 0.10.4 | MIT OR Apache-2.0 |
| `block-padding` | 0.3.3 | MIT OR Apache-2.0 |
| `bstr` | 1.12.3 | MIT OR Apache-2.0 |
| `bumpalo` | 3.20.3 | MIT OR Apache-2.0 |
| `byteorder` | 1.5.0 | Unlicense OR MIT |
| `bytes` | 1.12.0 | MIT |
| `calamine` | 0.36.1 | MIT |
| `cassowary` | 0.3.0 | MIT / Apache-2.0 |
| `castaway` | 0.2.4 | MIT |
| `cbc` | 0.1.2 | MIT OR Apache-2.0 |
| `cc` | 1.2.65 | MIT OR Apache-2.0 |
| `cff-parser` | 0.2.0 | MIT OR Apache-2.0 |
| `cfg-if` | 1.0.4 | MIT OR Apache-2.0 |
| `chacha20` | 0.10.2 | MIT OR Apache-2.0 |
| `cipher` | 0.4.4 | MIT OR Apache-2.0 |
| `codepage` | 0.1.2 | Apache-2.0 OR MIT |
| `colorchoice` | 1.0.5 | MIT OR Apache-2.0 |
| `compact_str` | 0.8.2 | MIT |
| `core_detect` | 1.0.0 | MIT/Apache-2.0 |
| `cpufeatures` | 0.2.17 | MIT OR Apache-2.0 |
| `cpufeatures` | 0.3.1 | MIT OR Apache-2.0 |
| `crc32fast` | 1.5.1 | MIT OR Apache-2.0 |
| `crossbeam-deque` | 0.8.6 | MIT OR Apache-2.0 |
| `crossbeam-epoch` | 0.9.18 | MIT OR Apache-2.0 |
| `crossbeam-utils` | 0.8.21 | MIT OR Apache-2.0 |
| `crossterm` | 0.28.1 | MIT |
| `crypto-common` | 0.1.7 | MIT OR Apache-2.0 |
| `darling` | 0.23.0 | MIT |
| `darling_core` | 0.23.0 | MIT |
| `darling_macro` | 0.23.0 | MIT |
| `debug_unsafe` | 0.1.4 | MIT OR Apache-2.0 |
| `defmt` | 1.1.1 | MIT OR Apache-2.0 |
| `defmt-macros` | 1.1.1 | MIT OR Apache-2.0 |
| `defmt-parser` | 1.0.0 | MIT OR Apache-2.0 |
| `digest` | 0.10.7 | MIT OR Apache-2.0 |
| `ecb` | 0.1.2 | MIT |
| `either` | 1.16.0 | MIT OR Apache-2.0 |
| `encoding_rs` | 0.8.40 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| `env_filter` | 2.0.0 | MIT OR Apache-2.0 |
| `env_logger` | 0.11.11 | MIT OR Apache-2.0 |
| `equivalent` | 1.0.2 | Apache-2.0 OR MIT |
| `errno` | 0.3.14 | MIT OR Apache-2.0 |
| `euclid` | 0.20.14 | MIT / Apache-2.0 |
| `fast-float2` | 0.2.4 | MIT OR Apache-2.0 |
| `find-msvc-tools` | 0.1.9 | MIT OR Apache-2.0 |
| `flate2` | 1.1.10 | MIT OR Apache-2.0 |
| `foldhash` | 0.1.5 | Zlib |
| `generic-array` | 0.14.7 | MIT |
| `getrandom` | 0.2.17 | MIT OR Apache-2.0 |
| `getrandom` | 0.4.3 | MIT OR Apache-2.0 |
| `globset` | 0.4.18 | Unlicense OR MIT |
| `hashbrown` | 0.15.5 | MIT OR Apache-2.0 |
| `hashbrown` | 0.17.1 | MIT OR Apache-2.0 |
| `heck` | 0.5.0 | MIT OR Apache-2.0 |
| `http` | 1.4.2 | MIT OR Apache-2.0 |
| `httparse` | 1.10.1 | MIT OR Apache-2.0 |
| `ident_case` | 1.0.1 | MIT/Apache-2.0 |
| `ignore` | 0.4.27 | Unlicense OR MIT |
| `indexmap` | 2.14.0 | Apache-2.0 OR MIT |
| `indoc` | 2.0.7 | MIT OR Apache-2.0 |
| `inout` | 0.1.4 | MIT OR Apache-2.0 |
| `instability` | 0.3.12 | MIT |
| `is_terminal_polyfill` | 1.70.2 | MIT OR Apache-2.0 |
| `itertools` | 0.13.0 | MIT OR Apache-2.0 |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 |
| `jiff` | 0.2.31 | Unlicense OR MIT |
| `lexopt` | 0.3.2 | MIT |
| `libc` | 0.2.186 | MIT OR Apache-2.0 |
| `linux-raw-sys` | 0.4.15 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `lock_api` | 0.4.14 | MIT OR Apache-2.0 |
| `log` | 0.4.33 | MIT OR Apache-2.0 |
| `lopdf` | 0.42.0 | MIT |
| `lru` | 0.12.5 | MIT |
| `md-5` | 0.10.6 | MIT OR Apache-2.0 |
| `memchr` | 2.8.2 | Unlicense OR MIT |
| `miniz_oxide` | 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| `mio` | 1.2.1 | MIT |
| `multiversion` | 0.8.0 | MIT OR Apache-2.0 |
| `multiversion-macros` | 0.8.0 | MIT OR Apache-2.0 |
| `multiversion_no_op` | 1.0.0 | Apache-2.0 OR MIT |
| `nom` | 8.0.0 | MIT |
| `num-traits` | 0.2.19 | MIT OR Apache-2.0 |
| `once_cell` | 1.21.4 | MIT OR Apache-2.0 |
| `parking_lot` | 0.12.5 | MIT OR Apache-2.0 |
| `parking_lot_core` | 0.9.12 | MIT OR Apache-2.0 |
| `paste` | 1.0.15 | MIT OR Apache-2.0 |
| `pdf-extract` | 0.12.0 | MIT |
| `percent-encoding` | 2.3.2 | MIT OR Apache-2.0 |
| `pom` | 1.1.0 | MIT |
| `postscript` | 0.14.1 | Apache-2.0/MIT |
| `proc-macro2` | 1.0.106 | MIT OR Apache-2.0 |
| `pulldown-cmark` | 0.13.4 | MIT |
| `quick-xml` | 0.41.0 | MIT |
| `quote` | 1.0.46 | MIT OR Apache-2.0 |
| `rand` | 0.10.2 | MIT OR Apache-2.0 |
| `rand_core` | 0.10.1 | MIT OR Apache-2.0 |
| `rangemap` | 1.8.0 | MIT/Apache-2.0 |
| `ratatui` | 0.29.0 | MIT |
| `regex` | 1.12.4 | MIT OR Apache-2.0 |
| `regex-automata` | 0.4.14 | MIT OR Apache-2.0 |
| `regex-syntax` | 0.8.11 | MIT OR Apache-2.0 |
| `ring` | 0.17.14 | Apache-2.0 AND ISC |
| `rust_xlsxwriter` | 0.99.0 | MIT OR Apache-2.0 |
| `rustix` | 0.38.44 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| `rustls` | 0.23.41 | Apache-2.0 OR ISC OR MIT |
| `rustls-pki-types` | 1.15.0 | MIT OR Apache-2.0 |
| `rustls-webpki` | 0.103.13 | ISC |
| `rustversion` | 1.0.22 | MIT OR Apache-2.0 |
| `ryu` | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| `same-file` | 1.0.6 | Unlicense/MIT |
| `scopeguard` | 1.2.0 | MIT OR Apache-2.0 |
| `serde` | 1.0.228 | MIT OR Apache-2.0 |
| `serde_core` | 1.0.228 | MIT OR Apache-2.0 |
| `serde_derive` | 1.0.228 | MIT OR Apache-2.0 |
| `serde_json` | 1.0.150 | MIT OR Apache-2.0 |
| `sha2` | 0.10.9 | MIT OR Apache-2.0 |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 |
| `signal-hook` | 0.3.18 | Apache-2.0/MIT |
| `signal-hook-mio` | 0.2.5 | MIT OR Apache-2.0 |
| `signal-hook-registry` | 1.4.8 | MIT OR Apache-2.0 |
| `simd-adler32` | 0.3.10 | MIT |
| `simdutf8` | 0.1.5 | MIT OR Apache-2.0 |
| `smallvec` | 1.15.2 | MIT OR Apache-2.0 |
| `static_assertions` | 1.1.0 | MIT OR Apache-2.0 |
| `stringprep` | 0.1.5 | MIT/Apache-2.0 |
| `strsim` | 0.11.1 | MIT |
| `strum` | 0.26.3 | MIT |
| `strum_macros` | 0.26.4 | MIT |
| `subtle` | 2.6.1 | BSD-3-Clause |
| `syn` | 2.0.118 | MIT OR Apache-2.0 |
| `target-features` | 0.1.6 | MIT OR Apache-2.0 |
| `thiserror` | 2.0.18 | MIT OR Apache-2.0 |
| `thiserror-impl` | 2.0.18 | MIT OR Apache-2.0 |
| `tinyvec` | 1.13.2 | Zlib OR Apache-2.0 OR MIT |
| `tinyvec_macros` | 0.1.1 | MIT OR Apache-2.0 OR Zlib |
| `ttf-parser` | 0.25.1 | MIT OR Apache-2.0 |
| `type1-encoding-parser` | 0.1.1 | MIT |
| `typed-path` | 0.12.3 | MIT OR Apache-2.0 |
| `typenum` | 1.20.1 | MIT OR Apache-2.0 |
| `unicase` | 2.9.0 | MIT OR Apache-2.0 |
| `unicode-bidi` | 0.3.18 | MIT OR Apache-2.0 |
| `unicode-ident` | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `unicode-normalization` | 0.1.25 | MIT OR Apache-2.0 |
| `unicode-properties` | 0.1.4 | MIT/Apache-2.0 |
| `unicode-segmentation` | 1.13.3 | MIT OR Apache-2.0 |
| `unicode-truncate` | 1.1.0 | MIT OR Apache-2.0 |
| `unicode-width` | 0.1.14 | MIT OR Apache-2.0 |
| `unicode-width` | 0.2.0 | MIT OR Apache-2.0 |
| `untrusted` | 0.9.0 | ISC |
| `ureq` | 3.3.0 | MIT OR Apache-2.0 |
| `ureq-proto` | 0.6.0 | MIT OR Apache-2.0 |
| `utf8-zero` | 0.8.1 | MIT OR Apache-2.0 |
| `utf8parse` | 0.2.2 | Apache-2.0 OR MIT |
| `version_check` | 0.9.5 | MIT/Apache-2.0 |
| `wait-timeout` | 0.2.1 | MIT/Apache-2.0 |
| `walkdir` | 2.5.0 | Unlicense/MIT |
| `webpki-roots` | 1.0.8 | CDLA-Permissive-2.0 |
| `weezl` | 0.1.12 | MIT OR Apache-2.0 |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT |
| `zip` | 8.6.0 | MIT |
| `zlib-rs` | 0.6.7 | Zlib |
| `zmij` | 1.0.21 | MIT |
| `zopfli` | 0.8.3 | Apache-2.0 |


## T54 note

T54 (documents and spreadsheets) took the tree from 112 crates to 177,
the largest single addition in the project's history. The five direct
additions and their licences:

| crate | version | licence | what it carries |
|---|---|---|---|
| `calamine` | 0.36.1 | MIT | reads xlsx / xlsm / xls / ods |
| `pdf-extract` | 0.12.0 | MIT | PDF text, pulls `lopdf` 0.42.0 (MIT) |
| `rust_xlsxwriter` | 0.99.0 | MIT OR Apache-2.0 | writes xlsx, with charts |
| `zip` | 8.6.0 | MIT | docx container |
| `quick-xml` | 0.41.0 | MIT | docx `word/document.xml` walk |

The other 60 are transitive. Nothing in the addition is a `*-sys` crate,
nothing compiles C, and both i686 targets build; that was the P0 spike's
first GO criterion and it held through P3.
