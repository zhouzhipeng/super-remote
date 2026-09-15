# 0.1.44 suspending the rest of the desktop effects

0.1.43 suspended transparency, which changed the taskbar from the grey acrylic
fallback to a flat light colour. A softer shadow remained along its top edge,
and it too followed focus.

Focus is what settles the diagnosis. Nothing in the capture path knows focus
exists - not GDI, not the encoder, not the tiles, not the browser - so a
difference that follows focus is the desktop itself, faithfully transmitted.
Acrylic falls back to a solid colour when its surface is not focused; an active
window casts a deeper shadow than an inactive one, and that shadow lands on the
taskbar. Both are by design and neither can be held in its focused state.

So the rest of the effects are suspended for the session as well: menu and
tooltip animation, fades, gradient captions, cursor and window shadows. This is
"adjust for best performance", and RDP does the same thing for the same reason -
each effect re-renders whenever something moves behind or beneath it, and every
one of those pixels is damage the refinement path encodes and the video carries.

It is applied through `SystemParametersInfo` (`SPI_SETUIEFFECTS` and
`SPI_SETDROPSHADOW`, with `SPIF_SENDCHANGE`) rather than by writing
`UserPreferencesMask`. The registry value is a binary bitmask that needs a
sign-out or an Explorer restart to take effect, which would mean restarting the
shell twice per connection; the API is what the Performance Options dialog
itself uses and applies live. `SPI_SETDROPSHADOW` is redundant with the master
switch on paper but is the setting that actually names window shadows, and the
two have drifted apart across Windows releases.

Each effect is read before it is changed and only recorded for restore when it
was actually changed - restoring something nobody touched is how a setting gets
lost. A user who already had an effect off is left alone in both directions. One
effect failing to suspend is logged and skipped rather than abandoning the
others. The guard stays reference counted with only its first holder reading the
user's settings, for the reason 0.1.43 gives.

`suspend_transparency` and `suspend_visual_effects` turn the two halves off
independently. Known limitation, unchanged: a host killed mid-session cannot
restore anything.

Verified by cross-compiling the module for `x86_64-pc-windows-msvc` in
isolation, which the full crate cannot do because of an unrelated C dependency.
That check is clean. Whether `SPI_SETUIEFFECTS` reaches a shadow DWM composes,
rather than only the classic Win32 effects it was defined for, is exactly what a
run on the host will show - it is the open question this change is testing.
