#!/usr/bin/env python3
"""Generate CSS custom properties from the design tokens (site/tokens.json).

Without selectors it writes the landing page's form: light on :root, dark
under prefers-color-scheme. With --light and --dark it writes the book's
form, keyed on mdbook's theme classes."""

import argparse
import json
import sys

HEADER = "/* generated from site/tokens.json by site/tools/gen_tokens.py; do not edit */"


def _declarations(tokens, mode):
    """Return (themed, plain) lists of (name, value) declarations.

    Themed colours take their value for `mode` ("light" or "dark"); plain
    tokens (the same in both) are returned separately so they are emitted
    only once.
    """
    themed, plain = [], []
    for tok in tokens["color"]["tokens"]:
        name, value = f"--ir-{tok['name']}", tok["value"]
        if isinstance(value, dict):
            themed.append((name, value[mode]))
        else:
            plain.append((name, value))
    typ = tokens["type"]
    for fam, stack in typ["families"].items():
        plain.append((f"--ir-font-{fam}", stack))
    for style in typ["scale"]:
        base = f"--ir-text-{style['name']}"
        plain.append((base, style["size"]))
        plain.append((f"{base}-line", style["lineHeight"]))
        if "letterSpacing" in style:
            plain.append((f"{base}-tracking", style["letterSpacing"]))
    for tok in tokens["radius"]["tokens"]:
        plain.append((f"--ir-{tok['name']}", tok["value"]))
    return themed, plain


def _block(selector, decls, indent=""):
    lines = [f"{indent}{selector} {{"]
    lines += [f"{indent}  {n}: {v};" for n, v in decls]
    lines.append(f"{indent}}}")
    return "\n".join(lines)


def _media_dark(selector, decls):
    return "@media (prefers-color-scheme: dark) {\n" + _block(selector, decls, "  ") + "\n}"


def tokens_css(tokens, light=None, dark=None):
    """CSS for the tokens. Light is the default on :root.

    With no selectors (the landing page), dark applies under
    prefers-color-scheme: dark. With both (the book), light also applies
    under `light`, dark under `dark`, and, when neither class is set (no
    JavaScript), under prefers-color-scheme: dark.
    """
    if (light is None) != (dark is None):
        raise ValueError("give both the light and the dark selector, or neither")
    light_themed, plain = _declarations(tokens, "light")
    dark_themed, _ = _declarations(tokens, "dark")
    if light is None:
        parts = [_block(":root", light_themed + plain), _media_dark(":root", dark_themed)]
    else:
        parts = [
            _block(f":root, {light}", light_themed + plain),
            _block(dark, dark_themed),
            _media_dark(f":root:not({light})", dark_themed),
        ]
    return "\n\n".join([HEADER] + parts) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("tokens", help="path to tokens.json")
    ap.add_argument("--light", help="selector for the light theme (the book)")
    ap.add_argument("--dark", help="selector for the dark theme (the book)")
    ap.add_argument("-o", "--output", required=True, help="output CSS file")
    args = ap.parse_args(argv)
    with open(args.tokens, encoding="utf-8") as f:
        tokens = json.load(f)
    try:
        css = tokens_css(tokens, args.light, args.dark)
    except ValueError as e:
        ap.error(str(e))
    with open(args.output, "w", encoding="utf-8") as f:
        f.write(css)
    return 0


if __name__ == "__main__":
    sys.exit(main())
