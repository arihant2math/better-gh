# Emoji assets

Served by `GET /api/v3/emojis` and `GET /_bgh/emoji/{code}.svg`.
Regenerate with `scripts/build-emoji-assets.mjs`.

* `twemoji.bin`: SVG graphics from Twemoji 15.0 (npm `@twemoji/svg`,
  community fork at https://github.com/jdecked/twemoji).
  Graphics copyright 2019 Twitter, Inc and other contributors, licensed
  under CC-BY 4.0 (https://creativecommons.org/licenses/by/4.0/). The
  `@twemoji/svg` packaging is MIT (copyright 2023 Samuel Kopp). The images
  are unmodified; they are stored gzip-compressed.
* `emojis.tsv`: gemoji shortcode names (npm `gemoji` 8.1, MIT, copyright
  Titus Wormer; names from github/gemoji, MIT, copyright GitHub, Inc.),
  mapped to the Twemoji file names.
