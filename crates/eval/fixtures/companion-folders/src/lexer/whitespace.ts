/**
 * removes only CommonMark horizontal whitespace from both boundaries
 * @param text syntax text
 * @returns remaining text
 */
export function trimSpace(text = ''): string {
  return trimSpaceEnd(trimSpaceStart(text));
}

/**
 * removes only trailing ASCII spaces and tabs
 * @param text syntax text
 * @returns remaining text
 */
export function trimSpaceEnd(text: string): string {
  let end = text.length;

  while (end && (text[end - 1] === ' ' || text[end - 1] === '\t')) {
    end--;
  }

  return text.slice(0, end);
}

/**
 * removes only leading ASCII spaces and tabs
 * @param text syntax text
 * @returns remaining text
 */
export function trimSpaceStart(text: string): string {
  let start = 0;

  while (text[start] === ' ' || text[start] === '\t') {
    start++;
  }

  return text.slice(start);
}
