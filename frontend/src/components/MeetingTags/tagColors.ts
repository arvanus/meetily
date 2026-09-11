export interface TagColor {
  key: string;
  label: string;
  /** Background, text and border classes for a chip. */
  chip: string;
  /** Solid fill for swatches and dots. */
  swatch: string;
}

// Class strings are written out in full, and this file lives under src/components,
// so Tailwind's content scan keeps them in the build
export const TAG_COLORS: TagColor[] = [
  { key: 'gray', label: 'Gray', chip: 'bg-gray-100 text-gray-700 border-gray-200', swatch: 'bg-gray-400' },
  { key: 'red', label: 'Red', chip: 'bg-red-100 text-red-700 border-red-200', swatch: 'bg-red-500' },
  { key: 'orange', label: 'Orange', chip: 'bg-orange-100 text-orange-700 border-orange-200', swatch: 'bg-orange-500' },
  { key: 'amber', label: 'Amber', chip: 'bg-amber-100 text-amber-800 border-amber-200', swatch: 'bg-amber-400' },
  { key: 'green', label: 'Green', chip: 'bg-green-100 text-green-700 border-green-200', swatch: 'bg-green-500' },
  { key: 'teal', label: 'Teal', chip: 'bg-teal-100 text-teal-700 border-teal-200', swatch: 'bg-teal-500' },
  { key: 'blue', label: 'Blue', chip: 'bg-blue-100 text-blue-700 border-blue-200', swatch: 'bg-blue-500' },
  { key: 'indigo', label: 'Indigo', chip: 'bg-indigo-100 text-indigo-700 border-indigo-200', swatch: 'bg-indigo-500' },
  { key: 'purple', label: 'Purple', chip: 'bg-purple-100 text-purple-700 border-purple-200', swatch: 'bg-purple-500' },
  { key: 'pink', label: 'Pink', chip: 'bg-pink-100 text-pink-700 border-pink-200', swatch: 'bg-pink-500' },
];

const DEFAULT_TAG_COLOR = TAG_COLORS[0];

/** Palette entry for a stored color key; unknown or missing keys render gray. */
export function tagColor(key: string | null | undefined): TagColor {
  return TAG_COLORS.find(color => color.key === key) ?? DEFAULT_TAG_COLOR;
}
