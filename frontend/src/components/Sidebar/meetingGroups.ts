import { format, isToday, isValid, isYesterday, parse, startOfDay } from 'date-fns';

export interface SidebarItem {
  id: string;
  title: string;
  type: 'folder' | 'file';
  children?: SidebarItem[];
}

export interface MeetingDateGroup {
  key: string;
  label: string;
  items: SidebarItem[];
}

const NO_DATE_KEY = 'no-date';

/**
 * The backend stores created_at as datetime('now', 'localtime'), i.e. "2026-07-17 14:30:00":
 * local time, space separator, no offset. That is not ISO 8601, and new Date() only parses
 * it by engine-specific leniency. Parse the known shape explicitly, fall back for anything else.
 */
export function parseMeetingDate(createdAt: string | undefined | null): Date | null {
  if (!createdAt) return null;

  const parsed = parse(createdAt, 'yyyy-MM-dd HH:mm:ss', new Date());
  if (isValid(parsed)) return parsed;

  const fallback = new Date(createdAt);
  return isValid(fallback) ? fallback : null;
}

export function formatDayLabel(date: Date, now: Date): string {
  if (isToday(date)) return 'Today';
  if (isYesterday(date)) return 'Yesterday';
  return format(date, isSameYear(date, now) ? 'd MMM' : 'd MMM yyyy');
}

function isSameYear(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear();
}

/**
 * Groups meeting items by calendar day, most recent day first, meetings within a day
 * newest first. The backend order is not relied upon. Items without a usable date are
 * never dropped: they land in a trailing "No date" group, keeping their original order.
 */
export function groupMeetingsByDay(
  items: SidebarItem[],
  dateById: Map<string, Date>,
  now: Date,
): MeetingDateGroup[] {
  const dated: Array<{ item: SidebarItem; date: Date }> = [];
  const undated: SidebarItem[] = [];

  items.forEach(item => {
    const date = dateById.get(item.id);
    if (date) dated.push({ item, date });
    else undated.push(item);
  });

  dated.sort((a, b) => b.date.getTime() - a.date.getTime());

  const groups: MeetingDateGroup[] = [];
  let current: MeetingDateGroup | null = null;
  let currentDay = '';

  dated.forEach(({ item, date }) => {
    const day = format(startOfDay(date), 'yyyy-MM-dd');
    if (!current || day !== currentDay) {
      current = { key: day, label: formatDayLabel(date, now), items: [] };
      currentDay = day;
      groups.push(current);
    }
    current.items.push(item);
  });

  if (undated.length > 0) {
    groups.push({ key: NO_DATE_KEY, label: 'No date', items: undated });
  }

  return groups;
}
