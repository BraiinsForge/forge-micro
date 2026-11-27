# Brands

The boards of the pair carry two brands. One directory here holds one
brand, and both directories live in this tree, because both trees build
boards that ship them.

| Brand | Boards |
|-------|--------|
| Forge Firmware | `rp2040`, `hashnode`, `forge-axe` |
| uBOS | every other board in either tree |

The brand is a board fact, so each board declares it in its own
`.cargo/config.toml`, beside that board's chip and log level:

```toml
[env]
UBOS_BRAND = { value = "../../brand/forge", relative = true }
```

`relative = true` resolves the value against the directory that holds
the `.cargo` directory, which is the crate directory, and not against
`.cargo/` itself. A board in the internal repository needs one more step
up, because this tree is a submodule of that one.

A nix build reads none of those files. Crane runs cargo from the flake
root, so a crate's own `.cargo/config.toml` is never an ancestor of the
directory cargo starts in. Each nix build therefore passes `UBOS_BRAND`
in the environment, from the board table in `nix/brands.nix`. The
declaration does not say `force = true`, so the environment wins, which
is what lets nix state the brand per board.

Two statements of one fact cannot import each other, so a check gates
their agreement. `brand-declaration`, from `nix/brand-gates.nix`, fails
when a board's `.cargo/config.toml` and `nix/brands.nix` name different
directories. This tree's call also fails when a brand directory is
incomplete; the internal repository reaches the same directories
through this submodule and leaves that half to the call here.

## brand.json

| Key | Use |
|-----|-----|
| `name` | the page title, and the `alt` text of the wordmark |
| `accent` | `--accent`, the brand colour |
| `accentHi` | `--accent-hi`, the hover colour |
| `accentText` | `--accent-text`, the text colour on an accent background |

Every key is required. `ubos/ubos-webui/build.rs` reads the file and
fails the build when a key is absent.

## The three files beside it

| File | Use |
|------|-----|
| `dashboard-logo.svg` | the wordmark of the dashboard, gzipped and served at `/dashboard-logo.svg` |
| `login-logo.svg` | the wordmark of the login page, gzipped and served at `/login-logo.svg` |
| `icon.png` | the tab icon, encoded into each page as a `data:` URI |

`build.rs` reads all three. A wordmark is the published asset, taken from
the brand's page on the design site and not redrawn. The icon is that
page's own tab icon, scaled down to 32x32 and not redrawn either.

Keep the icon inside the page. A board answers as few as two requests at
once, so a page that loads its icon from a path needs a third request
and loses its wordmark instead. A page that offers no icon at all is no
better: the browser then asks for `/favicon.ico` by itself.

Keep the icon at 32x32. Every byte of it is in both pages, both pages
are in flash, and `ubos/ubos-webui/src/tests/dashboard.rs` holds a size
budget over each one. The design site publishes each icon at 192x192
and at 512x512, and either one at full size puts both pages over
budget.

## Four rules a new brand must obey

**The wordmark uses one near-white fill.** The light theme flips the
wordmark to near-black with a CSS `invert()` filter. A single orange
fill obeys "one colour" and inverts to cyan, so the fill must be white
or a grey close to white. A wordmark of more than one colour needs a
different plan for the light theme.

**The wordmark is drawn 24px high or higher.** That is the smallest
height the design site permits on a screen, and `.logo img` in
`common.css` sets it.

**The wordmark is no wider than 260px at that height.** The height is
fixed and the width follows the file's aspect, so a wide lockup is what
pushes the nav links off the bar and out of the login card. The two
shipped wordmarks render 153px and 256px wide. `.logo img` caps the
width and scales a wider file down inside the cap rather than moving
the page, so a wordmark past the cap is shown too small, not rejected.

**`accentText` must reach 4.5:1 against `accent` and against
`accentHi`.** Three rules put text on an accent background, and two of
them lighten that background to `accentHi` on hover while the text
colour stays. So the text colour meets two backgrounds, not one, and
both pairs must clear WCAG AA for normal text. White on the Forge
orange `#eb6307` gives 3.32:1 and fails, which is why that brand puts
near-black text there instead. Measure both pairs before you choose
them: no gate measures them for you. `brand-declaration` checks that
each colour is a six-digit hex value and nothing more, because the
shipped uBOS brand fails this rule and a gate would refuse a palette
the design team has approved. The rule is a target until that is
resolved.

uBOS does not meet this rule on hover today. White on Violet 50
`#8b7cff` gives 3.27:1. The resting pair passes at 4.98:1, and the
failure is older than this file: the rules carried a literal `#fff`
before `accentText` existed, so the brand knob inherited the gap rather
than opening it. Fixing it means either a darker Violet 50 or a hover
text colour of its own, and both are design-team decisions. See the
section on uBOS and white text below.

## One file carries the whole wordmark

A page shows its own wordmark and nothing else beside it. A brand that
needs more words in its wordmark puts them in that file, because there
is no second element to put them in. The two pages can show different
files, and a brand that needs one mark on both pages ships two copies.

`name` is not the contents of a wordmark file, and the two are allowed to differ.
`name` is the product, which the page title and the wordmark `alt` text
carry. The wordmarks are the marks the design site publishes for that
product. uBOS is the case where they differ on purpose: the mark is the
Braiins wordmark, and the title says uBOS. Do not "correct" one to
match the other.

## Where each asset comes from

| Brand | Page |
|-------|------|
| Forge Firmware | `design.braiins.com/braiins-forge/logos/forge-firmware-logo` |
| uBOS | `design.braiins.com/braiins/logos/braiins-logo` |

Each page publishes the lockup for a light background and the lockup for
a dark background. Take the dark-background one: it is white, which is
what the `invert()` rule needs. The tab icon is the icon that page shows
in the browser tab.

The design site also holds the colours. The uBOS accent is Violet 60
and its hover colour is Violet 50, at
`design.braiins.com/braiins/colors`.

## uBOS keeps white text on its accent

The Braiins palette shows Violet 60 `#6b50ff` with black text. Black on
that violet gives 4.22:1, which fails the 4.5:1 rule above. White gives
4.98:1, so `accentText` is white. Ask the design team before you change
it.

White on Violet 50 `#8b7cff`, the hover background, gives only 3.27:1.
Black is the mirror of that and not an escape: it gives 6.42:1 on
Violet 50 and 4.22:1 on Violet 60. The two violets are far enough apart
that light text clears one and dark text clears the other, so no single
`accentText` clears both. This brand therefore needs a changed Violet
50 or a fourth colour for text on hover, and that choice belongs to the
design team rather than to this file.

## The Forge accent needs one more decision

`common.css` defines `--orange: #fe8431` for warning text, and the Forge
accent `#eb6307` sits close to it. The two measure 1.35:1 against each
other, which is the number behind "close": a pair that far apart is one
colour to a reader, not two. The design site makes it closer still:
`#eb6307` is Orange 50 there, and Orange 50 is also the warning colour.
A warning chip and a primary button must not read as the same colour.
Either the status orange moves, or somebody confirms on a real screen
that the two are far enough apart.
