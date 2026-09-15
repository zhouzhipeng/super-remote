const KEY = "remote-fullscreen";

// Storage is unavailable in some private-browsing contexts, and the writer runs
// inside a `fullscreenchange` handler that also drives reconnection - a throw
// there would cost far more than the preference is worth. Both sides fall back
// to "not fullscreen", which is the state a session starts in anyway.

export function fullscreenRemembered(): boolean {
  try {
    return localStorage.getItem(KEY) === "true";
  } catch {
    return false;
  }
}

export function rememberFullscreen(fullscreen: boolean): void {
  try {
    localStorage.setItem(KEY, String(fullscreen));
  } catch {
    /* not persisted */
  }
}
