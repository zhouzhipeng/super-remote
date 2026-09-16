export type ClipboardPasteRoute = "browser" | "host";

/**
 * Keeps remote in-session copy/paste from being overwritten by a stale browser
 * clipboard while still allowing text copied in another local app to cross the
 * clipboard data channel after this page loses focus.
 */
export class ClipboardShortcutRouter {
  /**
   * Set by a copy inside the remote session and held until focus leaves the
   * page, not consumed by the first paste that follows it.
   *
   * Copying once and pasting several times is ordinary, and the browser route
   * does not merely read the wrong clipboard - it pushes the browser's contents
   * to the Host, overwriting what was just copied there. The browser's own copy
   * of the Host clipboard cannot be trusted to stand in for it either: it is
   * read synchronously while the Ctrl+C keystroke is still being delivered, so
   * it often holds whatever the Host had *before* the copy.
   */
  #preferHost = false;
  #activePaste: ClipboardPasteRoute | null = null;

  get pasteActive(): boolean {
    return this.#activePaste !== null;
  }

  markRemoteCopy(): void {
    this.#preferHost = true;
  }

  beginPaste(): ClipboardPasteRoute {
    this.#activePaste ??= this.#preferHost ? "host" : "browser";
    return this.#activePaste;
  }

  endPaste(): ClipboardPasteRoute {
    const route = this.#activePaste ?? "browser";
    this.#activePaste = null;
    return route;
  }

  /** Focus left the page, so another application may have copied since. */
  reset(): void {
    this.#preferHost = false;
    this.#activePaste = null;
  }
}
