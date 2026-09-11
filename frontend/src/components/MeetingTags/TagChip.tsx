import { cn } from '@/lib/utils';
import { tagColor } from './tagColors';

interface TagChipProps {
  name: string;
  color: string | null | undefined;
  size?: 'sm' | 'md';
  className?: string;
}

export function TagChip({ name, color, size = 'sm', className }: TagChipProps) {
  return (
    <span
      title={name}
      className={cn(
        'inline-flex items-center rounded-full border font-medium truncate',
        size === 'sm' ? 'px-1.5 text-[10px] leading-4 max-w-[6.5rem]' : 'px-2 py-0.5 text-xs max-w-[12rem]',
        tagColor(color).chip,
        className,
      )}
    >
      {name}
    </span>
  );
}

/** Small solid dot in the tag color, for dense lists. */
export function TagColorDot({ color, className }: { color: string | null | undefined; className?: string }) {
  return <span className={cn('inline-block w-2.5 h-2.5 rounded-full shrink-0', tagColor(color).swatch, className)} />;
}
