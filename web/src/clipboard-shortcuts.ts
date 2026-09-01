export type ClipboardPasteRoute = "browser" | "host";

/**
 * Keeps remote in-session copy/paste from being overwritten by a stale browser
 * clipboard while still allowing text copied in another local app to cross the
 * clipboard data channel after this page loses focus.
 */
export class ClipboardShortcutRouter {
  #preferHostForNextPaste = false;
  #activePaste: ClipboardPasteRoute | null = null;

  get pasteActive(): boolean {
    return this.#activePaste !== null;
  }

  markRemoteCopy(): void {
    this.#preferHostForNextPaste = true;
  }

  beginPaste(): ClipboardPasteRoute {
    if (!this.#activePaste) {
      this.#activePaste = this.#preferHostForNextPaste ? "host" : "browser";
      this.#preferHostForNextPaste = false;
    }
    return this.#activePaste;
  }

  endPaste(): ClipboardPasteRoute {
    const route = this.#activePaste ?? "browser";
    this.#activePaste = null;
    return route;
  }

  reset(): void {
    this.#preferHostForNextPaste = false;
    this.#activePaste = null;
  }
}
