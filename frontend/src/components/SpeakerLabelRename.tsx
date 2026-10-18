"use client";

import { useState } from 'react';
import { Pencil, Loader2 } from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from './ui/popover';
import { Input } from './ui/input';
import { Button } from './ui/button';
import { invokeErrorMessage, isAutomaticSpeaker } from '@/lib/speaker';

interface SpeakerLabelRenameProps {
  speaker: string;
  label: string;
  className?: string;
  onRename: (speaker: string, name: string) => Promise<void>;
}

/** Speaker label in the transcript that opens a small rename field when clicked */
export function SpeakerLabelRename({ speaker, label, className, onRename }: SpeakerLabelRenameProps) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleOpenChange = (next: boolean) => {
    if (saving) return;
    setOpen(next);
    if (next) {
      setName(isAutomaticSpeaker(speaker) ? '' : label);
      setError(null);
    }
  };

  const save = async () => {
    const trimmed = name.trim();
    if (!trimmed || trimmed === label) {
      setOpen(false);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onRename(speaker, trimmed);
      setOpen(false);
    } catch (err) {
      setError(invokeErrorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Popover open={open} onOpenChange={handleOpenChange}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={`group inline-flex items-center gap-1 text-xs font-semibold hover:underline ${className ?? ''}`}
          title="Rename this speaker"
        >
          {label}
          <Pencil className="h-3 w-3 opacity-0 group-hover:opacity-60" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 p-3">
        <p className="text-xs text-muted-foreground mb-2">
          Rename &quot;{label}&quot; in every line of this meeting
        </p>
        <form
          className="flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <Input
            autoFocus
            value={name}
            placeholder="Name"
            onChange={(e) => setName(e.target.value)}
            disabled={saving}
            className="h-8"
          />
          <Button type="submit" size="sm" className="h-8" disabled={saving || !name.trim()}>
            {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : 'Save'}
          </Button>
        </form>
        {error && <p className="text-xs text-red-600 mt-2">{error}</p>}
      </PopoverContent>
    </Popover>
  );
}
