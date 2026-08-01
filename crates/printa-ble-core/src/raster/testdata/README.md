# URF test fixtures

`letter_600dpi.urf` was captured from macOS 26.5.2 by printing a two-line text
file to a `ippeveprinter -f image/urf` queue added with
`lpadmin -m everywhere`, so it is the output of Apple's own `rastertourf`
filter rather than something we synthesised.

It is US Letter at 600 dpi: 8 bpp grayscale, 5100 x 6600, one page, 22732
bytes with no trailing padding. Regenerate with the recipe in
[docs/AIRPRINT.md](../../../../../docs/AIRPRINT.md).
