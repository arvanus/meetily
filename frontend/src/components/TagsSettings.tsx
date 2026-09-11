'use client';

import { useCallback, useEffect, useState } from 'react';
import { Loader2, Pencil, Plus, Tag as TagIcon, Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Switch } from '@/components/ui/switch';
import { TagChip } from '@/components/MeetingTags/TagChip';
import { TagEditDialog } from '@/components/MeetingTags/TagEditDialog';
import {
  MeetingTag,
  TAGS_UPDATED_EVENT,
  TagInput,
  notifyTagsUpdated,
  tagService,
} from '@/services/tagService';

function errorMessage(error: unknown): string {
  return typeof error === 'string' ? error : error instanceof Error ? error.message : 'Unknown error';
}

export function TagsSettings() {
  const [tags, setTags] = useState<MeetingTag[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [autoTagEnabled, setAutoTagEnabled] = useState(false);
  const [editing, setEditing] = useState<{ open: boolean; tag: MeetingTag | null }>({ open: false, tag: null });
  const [deleting, setDeleting] = useState<MeetingTag | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);

  const loadTags = useCallback(async () => {
    try {
      setTags(await tagService.listTags());
    } catch (error) {
      console.error('Failed to load tags:', error);
      toast.error('Failed to load tags', { description: errorMessage(error) });
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadTags();
    tagService
      .getAutoTagEnabled()
      .then(setAutoTagEnabled)
      .catch((error) => console.error('Failed to load auto-tag setting:', error));

    // Tags created from a meeting's tag picker show up here too
    const onTagsUpdated = () => void loadTags();
    window.addEventListener(TAGS_UPDATED_EVENT, onTagsUpdated);
    return () => window.removeEventListener(TAGS_UPDATED_EVENT, onTagsUpdated);
  }, [loadTags]);

  const handleAutoTagChange = async (enabled: boolean) => {
    setAutoTagEnabled(enabled);
    try {
      await tagService.setAutoTagEnabled(enabled);
    } catch (error) {
      setAutoTagEnabled(!enabled);
      toast.error('Failed to save auto-tag setting', { description: errorMessage(error) });
    }
  };

  const handleSave = async (input: TagInput) => {
    try {
      if (editing.tag) {
        await tagService.updateTag(editing.tag.id, input);
        toast.success(`Tag "${input.name}" updated`);
      } else {
        await tagService.createTag(input.name, input.color, input.description);
        toast.success(`Tag "${input.name}" created`);
      }
      notifyTagsUpdated();
    } catch (error) {
      toast.error(editing.tag ? 'Failed to update tag' : 'Failed to create tag', {
        description: errorMessage(error),
      });
      throw error;
    }
  };

  const handleDelete = async () => {
    if (!deleting) return;
    setIsDeleting(true);
    try {
      await tagService.deleteTag(deleting.id);
      toast.success(`Tag "${deleting.name}" deleted`);
      setDeleting(null);
      notifyTagsUpdated();
    } catch (error) {
      toast.error('Failed to delete tag', { description: errorMessage(error) });
    } finally {
      setIsDeleting(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
        <div className="flex items-center justify-between gap-6">
          <div>
            <h3 className="text-lg font-semibold text-gray-900 mb-2">Auto-tag meetings</h3>
            <p className="text-sm text-gray-600">
              When a summary is generated, the AI picks the tags below that apply to the meeting.
              It only uses existing tags and replaces the meeting's current tags, including ones you set by hand.
            </p>
          </div>
          <Switch checked={autoTagEnabled} onCheckedChange={(checked) => void handleAutoTagChange(checked)} />
        </div>
      </div>

      <div className="bg-white rounded-lg border border-gray-200 p-6 shadow-sm">
        <div className="flex items-center justify-between mb-4">
          <div>
            <h3 className="text-lg font-semibold text-gray-900">Tags</h3>
            <p className="text-sm text-gray-600">Organize meetings and filter them in the sidebar.</p>
          </div>
          <Button onClick={() => setEditing({ open: true, tag: null })}>
            <Plus />
            New tag
          </Button>
        </div>

        {isLoading ? (
          <div className="flex items-center gap-2 py-6 text-sm text-gray-500">
            <Loader2 className="w-4 h-4 animate-spin" />
            Loading tags...
          </div>
        ) : tags.length === 0 ? (
          <div className="flex flex-col items-center py-10 text-center text-gray-500">
            <TagIcon className="w-8 h-8 mb-2 text-gray-300" />
            <p className="text-sm">No tags yet.</p>
            <p className="text-xs text-gray-400">Create tags here or from a meeting's summary.</p>
          </div>
        ) : (
          <ul className="divide-y divide-gray-100">
            {tags.map((tag) => (
              <li key={tag.id} className="flex items-center gap-4 py-3 group">
                <div className="w-44 shrink-0">
                  <TagChip name={tag.name} color={tag.color} size="md" />
                </div>
                <p className={`flex-1 min-w-0 text-sm truncate ${tag.description ? 'text-gray-600' : 'text-gray-300 italic'}`}>
                  {tag.description || 'No description'}
                </p>
                <span className="shrink-0 text-xs text-gray-400 tabular-nums">
                  {tag.meetingCount} {tag.meetingCount === 1 ? 'meeting' : 'meetings'}
                </span>
                <div className="flex items-center gap-1 shrink-0">
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Edit tag ${tag.name}`}
                    onClick={() => setEditing({ open: true, tag })}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Delete tag ${tag.name}`}
                    className="hover:text-red-600"
                    onClick={() => setDeleting(tag)}
                  >
                    <Trash2 />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      <TagEditDialog
        open={editing.open}
        tag={editing.tag}
        onOpenChange={(open) => setEditing((current) => ({ ...current, open }))}
        onSave={handleSave}
      />

      <Dialog open={deleting !== null} onOpenChange={(open) => !open && !isDeleting && setDeleting(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Delete tag?</DialogTitle>
            <DialogDescription>
              {deleting && deleting.meetingCount > 0
                ? `"${deleting.name}" will be removed from ${deleting.meetingCount} ${deleting.meetingCount === 1 ? 'meeting' : 'meetings'}. The meetings themselves are kept.`
                : `"${deleting?.name}" is not used by any meeting.`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2">
            <Button variant="outline" onClick={() => setDeleting(null)} disabled={isDeleting}>
              Cancel
            </Button>
            <Button variant="destructive" onClick={() => void handleDelete()} disabled={isDeleting}>
              {isDeleting && <Loader2 className="animate-spin" />}
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
