"use client";

import { useEffect, useState } from 'react';
import { Check, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { cn } from '@/lib/utils';
import { MeetingTag, TagInput } from '@/services/tagService';
import { TAG_COLORS } from './tagColors';
import { TagChip } from './TagChip';

interface TagEditDialogProps {
  open: boolean;
  /** Tag being edited; null creates a new one. */
  tag: MeetingTag | null;
  onOpenChange: (open: boolean) => void;
  onSave: (input: TagInput) => Promise<void>;
}

export function TagEditDialog({ open, tag, onOpenChange, onSave }: TagEditDialogProps) {
  const [name, setName] = useState('');
  const [color, setColor] = useState<string>(TAG_COLORS[0].key);
  const [description, setDescription] = useState('');
  const [isSaving, setIsSaving] = useState(false);

  useEffect(() => {
    if (!open) return;
    setName(tag?.name ?? '');
    setColor(tag?.color ?? TAG_COLORS[0].key);
    setDescription(tag?.description ?? '');
  }, [open, tag]);

  const trimmedName = name.trim();

  const handleSave = async () => {
    if (!trimmedName || isSaving) return;
    setIsSaving(true);
    try {
      await onSave({
        name: trimmedName,
        color,
        description: description.trim() || null,
      });
      onOpenChange(false);
    } catch {
      // The caller reports the error and the dialog stays open for a retry
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{tag ? 'Edit tag' : 'New tag'}</DialogTitle>
          <DialogDescription>
            The description helps the AI decide when this tag applies to a meeting.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="tag-name">Name</Label>
            <Input
              id="tag-name"
              value={name}
              autoFocus
              maxLength={60}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void handleSave();
              }}
              placeholder="e.g. Payzli"
            />
          </div>

          <div className="space-y-2">
            <Label>Color</Label>
            <div className="flex flex-wrap gap-2">
              {TAG_COLORS.map((option) => (
                <button
                  key={option.key}
                  type="button"
                  title={option.label}
                  aria-label={option.label}
                  aria-pressed={color === option.key}
                  onClick={() => setColor(option.key)}
                  className={cn(
                    'w-7 h-7 rounded-full flex items-center justify-center ring-offset-2 transition-shadow',
                    option.swatch,
                    color === option.key ? 'ring-2 ring-gray-900' : 'hover:ring-2 hover:ring-gray-300',
                  )}
                >
                  {color === option.key && <Check className="w-4 h-4 text-white" />}
                </button>
              ))}
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="tag-description">Description (optional)</Label>
            <Textarea
              id="tag-description"
              value={description}
              rows={3}
              maxLength={500}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="e.g. Meetings with the Payzli client about the payment integration"
            />
          </div>

          <div className="flex items-center gap-2 text-sm text-gray-500">
            <span>Preview:</span>
            <TagChip name={trimmedName || 'Tag'} color={color} size="md" />
          </div>
        </div>

        <DialogFooter className="gap-2">
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={isSaving}>
            Cancel
          </Button>
          <Button onClick={() => void handleSave()} disabled={!trimmedName || isSaving}>
            {isSaving && <Loader2 className="animate-spin" />}
            {tag ? 'Save' : 'Create'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
