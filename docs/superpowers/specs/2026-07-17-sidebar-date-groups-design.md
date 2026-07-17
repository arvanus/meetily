# Sidebar meeting date groups

## Problem

The sidebar meeting list (`frontend/src/components/Sidebar/index.tsx`) shows only the
meeting title. There is no way to tell when a meeting happened without opening it. The
sidebar is narrow, so any per-row date text competes with the title and the hover
actions.

The list already has date data available: `CurrentMeeting.created_at`
(`SidebarProvider.tsx:17`) and the `meetingDateById` map built for the date-range filter
(`index.tsx:266`).

## Approach

Group meeting rows under date headers ("Today", "Yesterday", "9 Jul 2026") and show only
the time (`14:30`) on each row. The header carries the date once per day instead of once
per meeting, so the vertical cost is amortized: a day with five meetings pays one header
line, not five date lines.

Rejected alternatives:

- Date on the right of each row: cheapest to build, but collides with the hover
  edit/delete buttons and repeats the same date on every row of the same day.
- Second line under the title: no hover collision, but grows every row by ~14px.
- Tooltip only: zero space, but undiscoverable and impossible to scan.

## Components

### `frontend/src/components/Sidebar/meetingGroups.ts` (new)

Pure module, no React. `index.tsx` is already ~800 lines; grouping logic does not belong
there.

`SidebarItem` is currently declared twice and exported nowhere — `index.tsx:40` and
`SidebarProvider.tsx:10` hold identical private copies. The new module needs the type, so
it becomes the single owner: `meetingGroups.ts` exports `SidebarItem`, and both existing
files delete their local copy and import it. This is the minimum needed to give the new
module a type to work with; no other type consolidation.

```ts
export interface SidebarItem {
  id: string;
  title: string;
  type: 'folder' | 'file';
  children?: SidebarItem[];
}

export interface MeetingDateGroup {
  key: string;              // stable React key, e.g. '2026-07-17' or 'no-date'
  label: string;            // 'Today' | 'Yesterday' | '9 Jul 2026' | 'No date'
  items: SidebarItem[];
}

export function parseMeetingDate(createdAt: string | undefined): Date | null;
export function formatDayLabel(date: Date, now: Date): string;
export function groupMeetingsByDay(
  items: SidebarItem[],
  dateById: Map<string, Date>,
  now: Date,
): MeetingDateGroup[];
```

Behavior:

- Groups by calendar day of `created_at`, most recent day first.
- Within a day, meetings are sorted by time descending. The backend does not guarantee
  order — `SidebarProvider.tsx:124` maps the raw array — so the grouping sorts itself.
- Items with no known date go into a single trailing group labeled `No date`, preserving
  their original relative order.
- `formatDayLabel` uses `isToday` / `isYesterday` / `format` from `date-fns` (already a
  dependency). Older days show `9 Jul`, dropping the year for the current year and
  showing `9 Jul 2025` only when it differs — the sidebar is 256px wide and the year is
  noise for the common case.

### Timestamp parsing

The backend writes `created_at` as `datetime('now', 'localtime')` (`backend/app/db.py:377`),
producing `"2026-07-17 14:30:00"` — a space separator and no UTC offset. That is not an
ISO 8601 string. Chromium (WebView2 on Windows) parses it leniently as local time, which
is the intended meaning, but WKWebView on macOS may return `Invalid Date`. The existing
`meetingDateById` map (`index.tsx:269`) already feeds that string to `new Date()` and
inherits the risk; showing `HH:mm` and grouping on day boundaries would make any
misparse plainly visible instead of silently widening a whole-day filter range.

`parseMeetingDate` therefore parses explicitly with
`parse(createdAt, 'yyyy-MM-dd HH:mm:ss', new Date())` from `date-fns`, treating the value
as local time, and falls back to `new Date(createdAt)` for any other shape (e.g. a real
ISO string from an older row). Returns `null` when the result is not a valid date.

`meetingDateById` (`index.tsx:266`) switches to `parseMeetingDate`, so the date-range
filter and the new grouping share one interpretation of the timestamp.

### `index.tsx` render changes

**Date headers replace the folder header** — the static "Meeting Notes" header row above
the list is removed, and the day headers become the only header level: two levels of
heading in a 256px sidebar is one too many, and the folder was never really collapsible
anyway (`index.tsx:96` force-expands it on every render). The list area maps
`meetingGroups` directly. Header styling: `text-xs font-medium text-gray-400 px-3 pt-2
pb-0.5`.

The `SidebarItem` folder shape in `SidebarProvider` stays as it is; the render flattens
folder children into the grouping. `expandedFolders` / `toggleFolder` are left untouched
— vestigial, but unwinding them is a separate change.

Two knock-on effects of dropping the folder header:

- The `Searching...` indicator lived in it. It moves under the search and date-filter
  controls, appearing only while a search is in flight.
- The header was the only thing rendered when the list was empty. An empty state takes
  its place: `No meetings match your filters` when a search or date filter is active,
  `No meetings yet` otherwise.

**Time on the row** — in the file row (`index.tsx:615`), between the title and the hover
action buttons:

```jsx
<span className="ml-2 shrink-0 text-xs text-gray-400 tabular-nums">14:30</span>
```

Omitted when the meeting has no known date. No layout shift on hover: the action buttons
already reserve their space via `opacity-0`, not `hidden`.

## Data flow

```
meetings (SidebarProvider)
   -> sidebarItems (folder 'meetings' + file children)
   -> filteredSidebarItems (index.tsx:286 — text search + date-range filter)
   -> groupMeetingsByDay(children, meetingDateById, new Date())
   -> [header, rows...] per day
```

Grouping consumes `filteredSidebarItems`, which is already the combined result of the
text search and the date-range filter. Consequence, and an explicit requirement: date
headers and row times appear during search exactly as they do in the unfiltered list;
days with no match simply drop out. The yellow `Match:` transcript snippet
(`index.tsx:643`) is unchanged and still renders under the title.

## Error handling

- Missing or unparseable `created_at`: the meeting is never hidden — it lands in the
  `No date` group. This mirrors `isWithinDateRange` (`index.tsx:279`), which also refuses
  to hide unknown-date meetings.
- Empty meeting list: no headers render; the folder renders as it does today.

## Testing

The frontend has no test runner (`package.json` has `lint` only, and ESLint is not
configured — `next lint` still opens its setup prompt). `npx tsc --noEmit` is the only
automated check. Verification is manual in the running app:

1. Meetings from several days group under correct headers, newest day first.
2. Today's and yesterday's meetings show `Today` / `Yesterday`, older ones show the date.
3. Row time matches the meeting's `created_at`.
4. Typing in the search box keeps headers and times; non-matching days disappear.
5. Applying a date-range filter keeps headers and times.
6. Hovering a row shows edit/delete without shifting the title or time.

## Out of scope

- `Today` / `Yesterday` are computed at render time. They do not self-refresh if the app
  stays open across midnight; the next re-render corrects them. No timer.
- No change to the date-range filter, the search behavior, or the match snippet.
