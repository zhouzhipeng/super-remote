# 0.1.26 audio endpoint recovery

Observed on 2026-09-14: the connected Host had opened Steam Streaming Speakers,
while the current console render endpoint was NVIDIA DigitalOutput. Panel state
reported mute requested=true but muted=false. Capture and mute policy both
retained endpoints acquired earlier in the session.

Changes:
- Loopback checks the default console render endpoint every 500 ms, including
  while silent. A changed endpoint causes capture to reopen.
- Audio supervisor retries after capture errors instead of exiting permanently;
  media-state changes interrupt capture. Capture worker ownership and COM teardown
  are explicit so cancelled streams do not leave a silent worker alive.
- Panel resolves the current default endpoint at each policy refresh and applies
  mute there, reporting SetMute failures instead of ignoring them on a cached object.
- Disconnect retains the existing policy of leaving local output muted.

Validation: production workspace tests and packaging passed. Installed 0.1.26
file hashes match its manifest; secrets/settings hashes are unchanged.
`web/tests/run-installed-audio-check.mjs` connected to the installed Host, clicked
the real sound toolbar button, played a generated 440 Hz tone through Windows,
and measured the decoded remote track without recording audio. Result: 323
received packets, 93507 bytes, peak RMS 0.0166, media unmuted and playing, current
host endpoint muted throughout. The second button click muted the Web element.
Browser output volume was zero to prevent this same-machine test feeding received
audio back into loopback; therefore this verifies decoded audio and button state,
not physical sound from the user's remote speakers. Disconnect left host muted.
Evidence: `.run/audio-0.1.26.log`.

The test does not programmatically switch the user's default device. Automatic
switch recovery is implemented but this run validates the current endpoint path.
