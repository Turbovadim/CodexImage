/** true when the event originates from a text field, so single-letter hotkeys must not fire */
export function isTyping(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null
  return !!t && (t.tagName === 'TEXTAREA' || t.tagName === 'INPUT' || t.isContentEditable)
}

/**
 * Layout-independent letter hotkey: matches the logical key, or the physical
 * key at that QWERTY position so shortcuts work on non-Latin layouts
 * (e.g. Cyrillic, where the D key produces "в").
 */
export function isKey(e: KeyboardEvent, letter: string): boolean {
  return e.key.toLowerCase() === letter || e.code === `Key${letter.toUpperCase()}`
}
