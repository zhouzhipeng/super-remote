# 0.1.43 suspending transparency effects

The Windows 11 taskbar rendered with a dark cast over a remote view. 0.1.42's
attempt to fix it by capturing after DWM composition was reverted: the
diagnosis was wrong. A capture artefact cannot depend on focus, and the
behaviour does — the taskbar is normal while focused and dark while not.

Acrylic and Mica fall back to a solid colour whenever their surface is not
focused. That is by design, so those pixels really are on screen, and the
taskbar genuinely alternates between two appearances as focus moves. There is no
supported way to hold acrylic in its focused state: XAML exposes only
`AcrylicBrush.AlwaysUseFallback`, which forces the opposite. Forcing focus is
not an option either — it would take focus from whatever is being used.

So the alternation is removed by removing the effect. While a client is
connected the host writes `EnableTransparency = 0` under the Personalize key and
broadcasts `WM_SETTINGCHANGE`/`ImmersiveColorSet`; on disconnect it puts back
exactly what was there, deleting the value again if it never existed, since
absent is how Windows spells "enabled". A user who already had the effect off is
left alone in both directions.

This is also what a remote session wants for its own reasons. A blurred backdrop
re-renders whenever anything moves behind it, and every one of those pixels is
damage the refinement path has to encode and the video has to carry. RDP
disables desktop effects for the same reason.

The guard is reference counted and only its first holder reads the user's
setting. A later one would read the value this module just wrote and restore
*that* on release, which would turn a session-scoped change into a permanent
one. It is taken once per session rather than per video stream, because the
encoder restarts on display and viewport changes and toggling the effect on each
of those would make the desktop flicker. A failure to suspend is logged and
ignored: a session is worth more than the effect.

`suspend_transparency` turns the whole behaviour off. Known limitation: a host
killed mid-session cannot restore the setting, and it stays off until the next
session ends or it is changed by hand.

Verified by cross-compiling the module for `x86_64-pc-windows-msvc` in
isolation, which the full crate cannot do because of an unrelated C dependency.
That check is clean. It proves the code builds, not that the registry write has
the intended effect on a live desktop — that still needs a run on the host.
