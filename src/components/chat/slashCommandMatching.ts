export interface SlashSearchItem {
  id: string;
  name: string;
  description: string;
  group?: string;
  searchTerms?: string[];
}

const CLASSIC_SLASH_TOKEN_PATTERN = /(?:^|\s)\/([^\s/]*)$/;

function normalizeSearchText(value: string): string {
  return value.trim().toLocaleLowerCase();
}

function fuzzyScore(query: string, candidate: string): number | null {
  const normalizedCandidate = normalizeSearchText(candidate);
  if (!normalizedCandidate) {
    return null;
  }

  const directIndex = normalizedCandidate.indexOf(query);
  if (directIndex >= 0) {
    return 10_000 - directIndex * 10 - normalizedCandidate.length;
  }

  let cursor = 0;
  let score = 0;
  let previousIndex = -2;
  for (const character of query) {
    const matchIndex = normalizedCandidate.indexOf(character, cursor);
    if (matchIndex < 0) {
      return null;
    }
    score += matchIndex === previousIndex + 1 ? 20 : 1;
    previousIndex = matchIndex;
    cursor = matchIndex + 1;
  }

  return score;
}

export function findClassicSlashQuery(value: string, cursorPosition: number): string | null {
  const textBeforeCursor = value.slice(0, cursorPosition);
  const match = CLASSIC_SLASH_TOKEN_PATTERN.exec(textBeforeCursor);
  return match ? match[1] ?? "" : null;
}

export function removeClassicSlashToken(
  value: string,
  cursorPosition: number,
): { value: string; cursorPosition: number } {
  const textBeforeCursor = value.slice(0, cursorPosition);
  if (!CLASSIC_SLASH_TOKEN_PATTERN.test(textBeforeCursor)) {
    return { value, cursorPosition };
  }

  const slashPosition = textBeforeCursor.lastIndexOf("/");
  const characterBeforeSlash = value[slashPosition - 1];
  const characterAfterToken = value[cursorPosition];
  const removeTrailingWhitespace =
    Boolean(characterAfterToken && /\s/.test(characterAfterToken)) &&
    (slashPosition === 0 ||
      Boolean(characterBeforeSlash && /\s/.test(characterBeforeSlash)));
  const tokenEnd = removeTrailingWhitespace ? cursorPosition + 1 : cursorPosition;

  return {
    value: `${value.slice(0, slashPosition)}${value.slice(tokenEnd)}`,
    cursorPosition: slashPosition,
  };
}

export function filterClassicSlashItems<T extends SlashSearchItem>(
  items: T[],
  query: string,
): T[] {
  const normalizedQuery = normalizeSearchText(query);
  if (!normalizedQuery) {
    return items;
  }

  const groupOrder = new Map<string, number>();
  for (const item of items) {
    const group = item.group ?? "";
    if (!groupOrder.has(group)) {
      groupOrder.set(group, groupOrder.size);
    }
  }

  return items
    .map((item, index) => {
      const terms = [item.name, item.id, item.description, ...(item.searchTerms ?? [])];
      const score = Math.max(
        ...terms.map((term) => fuzzyScore(normalizedQuery, term) ?? Number.NEGATIVE_INFINITY),
      );
      return { item, index, score };
    })
    .filter((entry) => Number.isFinite(entry.score))
    .sort((left, right) => {
      const groupDifference =
        (groupOrder.get(left.item.group ?? "") ?? 0) -
        (groupOrder.get(right.item.group ?? "") ?? 0);
      if (groupDifference !== 0) {
        return groupDifference;
      }
      return right.score - left.score || left.index - right.index;
    })
    .map((entry) => entry.item);
}
