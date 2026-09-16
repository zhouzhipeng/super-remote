# 0.1.43 a copy on the Host survives more than one paste

Copying on the Host and then pasting worked once. Every Ctrl+V after that
pushed the browser machine's clipboard across instead, which does not merely
paste the wrong text - it overwrites the Host clipboard, so what was copied
there is gone and no amount of repeating the paste recovers it.

`ClipboardShortcutRouter` consumed its Host preference on the first paste:

    this.#activePaste = this.#preferHostForNextPaste ? "host" : "browser";
    this.#preferHostForNextPaste = false;

The preference is now held until focus leaves the page, which is the event that
actually means another application could have copied something. Copying once
and pasting several times is ordinary, and nothing about the first paste says
the Host's clipboard has stopped being the right source.

The browser's own copy of the Host clipboard cannot stand in for it. It is read
by a synchronous request issued while the Ctrl+C keystroke is still being
delivered to the Host, so it frequently holds whatever the Host had *before*
the copy - which is why the Host route exists at all, and why it has to outlast
a single paste.

Tests: one copy serves five consecutive pastes; a copy after focus returns takes
the route back; a paste already in flight keeps its route when a copy lands
underneath it. The five-paste test fails against the previous behaviour, which
is what makes it worth having. The existing first-paste and repeated-keydown
cases are unchanged, minus the assertion that encoded the bug.
