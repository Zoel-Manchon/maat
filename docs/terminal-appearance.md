# Recommended appearance

Maat is a TUI application: it draws colours and styles, but **the typeface is
controlled by the terminal emulator**. The binary cannot change it portably.

## Chosen typeface: Departure Mono

Departure Mono is a pixel monospace font inspired by command-line interfaces,
early GUIs and the small typefaces of the late 90s and early 2000s. It suits
Maat's ink-and-wine identity without falling back on the phosphor-green
cliché.

Recommended settings:

- Size: 13–15 pt.
- Weight: Regular.
- Line height: 1.0–1.1.
- Ligatures: off.
- Terminal opacity: 100%.

For long sessions or low-density displays, **IBM Plex Mono** is a more
conservative and legible alternative that keeps a technical, mechanical feel.

## Windows Terminal

After installing the font on Windows, add this to the profile you use for
Maat:

```json
{
  "font": {
    "face": "Departure Mono",
    "size": 13
  }
}
```

## VS Code integrated terminal

```json
{
  "terminal.integrated.fontFamily": "'Departure Mono', 'IBM Plex Mono', monospace",
  "terminal.integrated.fontLigatures.enabled": false,
  "terminal.integrated.fontSize": 13
}
```

## Linux

Most terminals let you pick `Departure Mono` under Preferences → Profile →
Text/Font. Turn off "use the system font" if that option is present.

## Oxblood palette

| Role | Hex | Used for |
|---|---:|---|
| Ink | `#14090B` | Main background |
| Panel | `#221114` | Status bar |
| Current | `#1F0F12` | Active line |
| Overlay | `#1A0C0F` | Help panel |
| Bone | `#E8D8CE` | Body text |
| Ivory | `#F5EDE6` | Current line and emphasis |
| Dim | `#9A7070` | Line numbers |
| Faint | `#3A2225` | Tildes past end of buffer |
| Wine | `#B03A48` | Normal mode tag |
| Rose | `#D96A78` | Command mode tag |
| Gold | `#E0A458` | Warnings, search, modified marker |
| Red | `#F05D6C` | Errors |
| Dusty rose | `#C48B94` | SHA-256 |

### Terminals without true colour

Appliance consoles often expose only the 16 ANSI colours. Maat checks
`COLORTERM` at startup and, when true colour is not advertised, every colour
degrades to its nearest indexed equivalent rather than rendering as mud. Nothing
needs configuring — but if a capable terminal renders flat, check that
`COLORTERM` is set to `truecolor` or `24bit`.
