import { invoke } from '@tauri-apps/api/core';

export interface MeetingTag {
  id: string;
  name: string;
  color: string | null;
  description: string | null;
  meetingCount: number;
}

/** The part of a tag that meeting lists carry to render chips. */
export interface MeetingTagSummary {
  id: string;
  name: string;
  color: string | null;
}

export interface MeetingListItem {
  id: string;
  title: string;
  created_at: string;
  tags: MeetingTagSummary[];
}

export interface TagInput {
  name: string;
  color: string | null;
  description: string | null;
}

/** Fired on window whenever tags or meeting tag assignments change. */
export const TAGS_UPDATED_EVENT = 'meetily-tags-updated';

export function notifyTagsUpdated() {
  window.dispatchEvent(new CustomEvent(TAGS_UPDATED_EVENT));
}

export class TagService {
  async listTags(): Promise<MeetingTag[]> {
    return invoke<MeetingTag[]>('api_list_tags');
  }

  async createTag(name: string, color: string | null = null, description: string | null = null): Promise<MeetingTag> {
    return invoke<MeetingTag>('api_create_tag', { name, color, description });
  }

  async updateTag(tagId: string, input: TagInput): Promise<MeetingTag> {
    return invoke<MeetingTag>('api_update_tag', { tagId, ...input });
  }

  async deleteTag(tagId: string): Promise<void> {
    return invoke<void>('api_delete_tag', { tagId });
  }

  async getMeetingTags(meetingId: string): Promise<MeetingTag[]> {
    return invoke<MeetingTag[]>('api_get_meeting_tags', { meetingId });
  }

  async setMeetingTags(meetingId: string, tagIds: string[]): Promise<MeetingTag[]> {
    return invoke<MeetingTag[]>('api_set_meeting_tags', { meetingId, tagIds });
  }

  async getMeetingsForTag(tagId: string): Promise<MeetingListItem[]> {
    return invoke<MeetingListItem[]>('api_get_meetings_for_tag', { tagId });
  }

  async getAutoTagEnabled(): Promise<boolean> {
    return invoke<boolean>('api_get_auto_tag_setting');
  }

  async setAutoTagEnabled(enabled: boolean): Promise<void> {
    return invoke<void>('api_set_auto_tag_setting', { enabled });
  }
}

export const tagService = new TagService();
