// Keyboard behavior of the button rows marked role="radiogroup" (page filter, display mode,
// job operation).

const STEPS: Record<string, number> = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 };

/**
 * Keydown handler for a role="radiogroup": arrow keys, Home and End check and focus another
 * radio. Radios take part in Tab order only while checked (tabIndex 0, the others -1).
 */
export function onRadioGroupKey(event: KeyboardEvent): void {
  const radios = [...(event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('[role="radio"]')];
  const at = radios.indexOf(event.target as HTMLElement);
  if (at < 0) return;
  const step = STEPS[event.key];
  const next =
    event.key === "Home"
      ? 0
      : event.key === "End"
        ? radios.length - 1
        : step === undefined
          ? null
          : (at + step + radios.length) % radios.length;
  if (next === null) return;
  event.preventDefault();
  radios[next]!.focus();
  radios[next]!.click();
}
