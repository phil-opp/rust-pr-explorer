# Iosevka (subset)

The default code font, embedded in every generated page so that
it works offline.

- Source: `@fontsource/iosevka` 5.3.0 on npm (`files/iosevka-latin-{400,700}-normal.woff2`).
- License: SIL Open Font License 1.1, see `LICENSE-Iosevka.txt`.
- Subset to the characters the page shows in code (ASCII, Latin-1 and a few
  punctuation marks and arrows), which takes each weight from about 1 MB to 23 kB:

```sh
pyftsubset iosevka-latin-400-normal.woff2 --flavor=woff2 --output-file=iosevka-400.woff2 \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2013,U+2014,U+2018,U+2019,U+201C,U+201D,U+2022,U+2026,U+2190-2193,U+2212,U+25B8,U+25C2,U+25BE,U+00D7"
```

(and the same for weight 700). Characters outside the subset fall back to the
next font in the stack.
