#!/usr/bin/env python3
"""Generate CSS custom properties from the design tokens (site/tokens.json)."""

import argparse
import json
import re
import sys

HEADER = "/* generated from site/tokens.json by site/tools/gen_tokens.py; do not edit */"
ALIAS = re.compile(r"^\{([^}]+)\}$")


def _resolve(value):
    m = ALIAS.match(value)
    return f"var(--ir-{m.group(1)})" if m else value


def _declarations(tokens, theme):
    """Return (themed, plain) lists of (name, value) declarations.

    Themed tokens take their value for `theme`; plain tokens (the same in
    every theme) are returned separately so they are emitted only once.
    """
    themed, plain = [], []
    for family in (tokens["color"], tokens["spacing"], tokens["radius"], tokens["shadow"]):
        for tok in family["tokens"]:
            name = f"--ir-{tok['name']}"
            value = tok["value"]
            if isinstance(value, dict):
                themed.append((name, _resolve(value[theme])))
            else:
                plain.append((name, _resolve(value)))
    typ = tokens["type"]
    for fam, stack in typ["families"].items():
        plain.append((f"--ir-font-{fam}", stack))
    for group in typ["groups"]:
        for style in group["styles"]:
            base = f"--ir-{style['name']}"
            plain.append((f"{base}-size", style["fontSize"]))
            plain.append((f"{base}-line", style["lineHeight"]))
            plain.append((f"{base}-weight", str(style["fontWeight"])))
            if "letterSpacing" in style:
                plain.append((f"{base}-tracking", style["letterSpacing"]))
    return themed, plain


def _block(selector, decls, indent=""):
    lines = [f"{indent}{selector} {{"]
    lines += [f"{indent}  {n}: {v};" for n, v in decls]
    lines.append(f"{indent}}}")
    return "\n".join(lines)


def tokens_css(tokens, light_selector, dark_selector):
    """CSS with dark as the :root default and light under `light_selector`
    and, absent an explicit dark choice, under prefers-color-scheme: light."""
    default, other = (t["id"] for t in tokens["color"]["themes"][:2])
    dark_themed, plain = _declarations(tokens, default)
    light_themed, _ = _declarations(tokens, other)
    parts = [
        HEADER,
        _block(f":root, {dark_selector}", dark_themed + plain),
        _block(light_selector, light_themed),
        "@media (prefers-color-scheme: light) {\n"
        + _block(f":root:not({dark_selector})", light_themed, "  ")
        + "\n}",
    ]
    return "\n\n".join(parts) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("tokens", help="path to tokens.json")
    ap.add_argument("--light", required=True, help="selector for the light theme")
    ap.add_argument("--dark", required=True, help="selector for the dark theme")
    ap.add_argument("-o", "--output", required=True, help="output CSS file")
    args = ap.parse_args(argv)
    with open(args.tokens, encoding="utf-8") as f:
        tokens = json.load(f)
    with open(args.output, "w", encoding="utf-8") as f:
        f.write(tokens_css(tokens, args.light, args.dark))
    return 0


if __name__ == "__main__":
    sys.exit(main())
