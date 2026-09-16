# 0.1.43 suspending the effects that follow focus

The Windows 11 taskbar changes appearance with focus, and over a remote view the
darker state reads as a shadow that should not be there. Two effects do it, both
by design, and neither can be held in its focused state:

- acrylic falls back to a solid colour when its surface is not focused;
- an active window casts a deeper shadow than an inactive one, and a window
  sitting near the bottom of the screen casts it onto the taskbar.

Both are suspended for the life of a session and restored on disconnect. A
remote session wants them gone anyway: each re-renders whenever something moves
behind or beneath it, and every one of those pixels is damage the refinement
path encodes and the video carries. RDP disables desktop effects for the same
reason - though it gets it for free, because it renders its own session rather
than capturing this one.

## Why the previous attempt did nothing

An earlier version called `SPI_SETDROPSHADOW` and had no effect. The API was
right - it owns bit 0x20 of byte 1 of `UserPreferencesMask`, which is the
Performance Options checkbox - but the call was wrong in two ways:

- it passed only `SPIF_SENDCHANGE`. `SPIF_UPDATEINIFILE` is what records the
  change in the user's preferences; without it there was nothing to keep.
- it never selected the custom visual effects mode. An individual effect is
  overridden under "let Windows choose", so the bit was set and then ignored.

`VisualFXSetting` is now set to 3 (Custom) before the shadow is touched, and
restored after it on release - putting the effect back while the mode still says
custom is what the user had, where flipping the mode first would leave a window
in which neither value describes their configuration.

The same version also called `SPI_SETUIEFFECTS`, a broad switch over the classic
Win32 effects. It did nothing here and is not back: it changes far more than was
asked for, to no observed benefit.

## Shape

Each value is read before it is changed and only recorded for restore when it
was actually changed - restoring something nobody touched is how a setting gets
lost. A user who already had an effect off, or already had custom mode selected,
is left alone in both directions. A registry value that did not exist is
restored by deleting it again rather than by inventing a number they never set.
One effect failing is logged and skipped rather than abandoning the others.

The guard is reference counted and only its first holder reads the user's
settings; a later one would read what this module wrote and restore *that*,
turning a session-scoped change permanent. It is taken once per session rather
than per video stream, because the encoder restarts on display and viewport
changes.

`suspend_transparency` and `suspend_window_shadows` turn the two halves off
independently. Known limitation: a host killed mid-session cannot restore
anything.

## Verification

Cross-compiled for `x86_64-pc-windows-msvc` in isolation, which the full crate
cannot do because of an unrelated C dependency; that check is clean. The
`Win32_System_Registry` feature is declared in `host/Cargo.toml` this time - the
previous attempt verified the module against a scratch manifest of its own and
so never noticed the real one was missing it.

What this does not establish is whether the setting reaches the shadow DWM
composes on Windows 11. That is what a run on the host will show. If it does
not, the remaining evidence points at the artefact being in one of the two pixel
layers rather than in Windows at all, and the measurement for that is the
toolbar's own text: it begins "静止无损补偿" on the sharp layer and "FPS ..." on
the video layer.
