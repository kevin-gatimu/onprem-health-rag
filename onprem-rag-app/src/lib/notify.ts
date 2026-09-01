// Background-completion notifications. Toasts already cover the foreground case,
// so an OS notification fires ONLY when the window is hidden/unfocused — one
// signal per event, never both.
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';

let focused = true;
window.addEventListener('focus', () => { focused = true; });
window.addEventListener('blur',  () => { focused = false; });

/** Permission result, memoized after the first (possibly prompting) check. */
let permitted: Promise<boolean> | null = null;
function ensurePermission(): Promise<boolean> {
  permitted ??= (async () => {
    if (await isPermissionGranted()) return true;
    return (await requestPermission()) === 'granted';
  })();
  return permitted;
}

/**
 * Notify about a finished background job — no-op while the window is focused and
 * visible (the toast covers that). Failures are swallowed: notifications are
 * best-effort and must never break the calling flow.
 */
export async function notifyBackground(title: string, body: string): Promise<void> {
  if (focused && !document.hidden) return;
  try {
    if (await ensurePermission()) sendNotification({ title, body });
  } catch {
    /* unsupported platform or denied — the in-app toast already fired */
  }
}
