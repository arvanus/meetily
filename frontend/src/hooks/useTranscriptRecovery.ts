/**
 * useTranscriptRecovery Hook
 *
 * Orchestrates recovery of recordings that were interrupted before they could be
 * saved. Detects, previews and recovers them.
 *
 * Discovery reads SQLite: every recording (desktop app and CLI) inserts its
 * meeting row when it STARTS, flagged `status = 'recording'`, and completes it on
 * a clean stop. A row left at `'recording'` is an interrupted session, whatever
 * process was running it - which is why this no longer reads IndexedDB, a store
 * the headless CLI cannot write to.
 *
 * IndexedDB remains as a fallback for the one case that leaves nothing on disk:
 * a recording with auto-save turned off, which writes no transcripts.json. Both
 * stores key on the same meeting id, so the fallback is a direct lookup.
 */

import { useState, useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { indexedDBService, MeetingMetadata, StoredTranscript } from '@/services/indexedDBService';

/** An interrupted recording, as reported by the backend. */
interface IncompleteMeeting {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  folder_path?: string | null;
  transcript_count: number;
  has_audio: boolean;
  is_live: boolean;
}

/**
 * How long to wait before re-checking a recording that was still heartbeating.
 * The backend calls a recording live for 90s after its last heartbeat, so one
 * re-check past that window tells a crash-then-restart apart from a CLI session
 * that is genuinely still running.
 */
const LIVE_RECHECK_MS = 95_000;

/** Outcome of recovering one meeting. */
export interface RecoveryResult {
  meeting_id: string;
  title: string;
  transcript_count: number;
  audio_status: string; // "success" | "partial" | "failed" | "none"
  audio_file_path?: string | null;
  message: string;
}

/** What a caller of `recoverMeeting` gets back. */
export interface RecoveryOutcome {
  success: boolean;
  /** Audio merge result: "success" | "partial" | "failed" | "none". */
  audioStatus: string;
  meetingId: string;
}

export interface UseTranscriptRecoveryReturn {
  recoverableMeetings: MeetingMetadata[];
  isLoading: boolean;
  isRecovering: boolean;
  checkForRecoverableTranscripts: () => Promise<void>;
  recoverMeeting: (meetingId: string) => Promise<RecoveryOutcome>;
  loadMeetingTranscripts: (meetingId: string) => Promise<StoredTranscript[]>;
  deleteRecoverableMeeting: (meetingId: string) => Promise<void>;
}

export function useTranscriptRecovery(): UseTranscriptRecoveryReturn {
  const [recoverableMeetings, setRecoverableMeetings] = useState<MeetingMetadata[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isRecovering, setIsRecovering] = useState(false);
  const recheckTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (recheckTimer.current) clearTimeout(recheckTimer.current);
  }, []);

  /**
   * Check for interrupted recordings.
   *
   * Rows still heartbeating are held back rather than shown, and re-checked once
   * their heartbeat has had time to go stale - a recording that crashed seconds
   * before the app opened is indistinguishable from a live one until then.
   */
  const checkForRecoverableTranscripts = useCallback(async () => {
    setIsLoading(true);
    try {
      const all = await invoke<IncompleteMeeting[]>('api_get_incomplete_meetings');
      const incomplete = all.filter(m => !m.is_live);

      if (all.length !== incomplete.length) {
        console.log(`${all.length - incomplete.length} recording(s) still active, re-checking shortly`);
        if (recheckTimer.current) clearTimeout(recheckTimer.current);
        recheckTimer.current = setTimeout(() => {
          checkForRecoverableTranscripts();
        }, LIVE_RECHECK_MS);
      }

      const mapped: MeetingMetadata[] = await Promise.all(
        incomplete.map(async (meeting) => {
          // A recording with auto-save off wrote nothing to disk; its transcripts
          // only exist in IndexedDB, under this same meeting id.
          let transcriptCount = meeting.transcript_count;
          if (transcriptCount === 0) {
            try {
              transcriptCount = await indexedDBService.getTranscriptCount(meeting.id);
            } catch (error) {
              console.warn('IndexedDB fallback count failed:', error);
            }
          }

          return {
            meetingId: meeting.id,
            title: meeting.title,
            startTime: Date.parse(meeting.created_at),
            lastUpdated: Date.parse(meeting.updated_at),
            transcriptCount,
            savedToSQLite: false,
            // The dialog reads this as "audio available", so only advertise a
            // folder that still holds checkpoints to merge.
            folderPath: meeting.has_audio ? meeting.folder_path ?? undefined : undefined,
          };
        })
      );

      // Drop rows with nothing to recover anywhere - a recording that died before
      // producing a single segment or checkpoint. Offering those would just mint
      // empty meetings. The row stays in the database, harmless and out of every
      // list, until a later run has something to offer.
      const withContent = mapped.filter(m => m.transcriptCount > 0 || m.folderPath);
      if (withContent.length !== mapped.length) {
        console.log(`${mapped.length - withContent.length} interrupted recording(s) had no data to recover`);
      }

      setRecoverableMeetings(withContent);
    } catch (error) {
      console.error('Failed to check for interrupted recordings:', error);
      setRecoverableMeetings([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  /**
   * Load transcripts for preview, from disk with an IndexedDB fallback.
   */
  const loadMeetingTranscripts = useCallback(async (meetingId: string): Promise<StoredTranscript[]> => {
    try {
      const segments = await invoke<any[]>('api_get_incomplete_meeting_transcripts', { meetingId });

      if (segments.length > 0) {
        return segments.map((s, index) => ({
          meetingId,
          text: s.text,
          timestamp: s.timestamp,
          confidence: 1,
          sequenceId: index,
          storedAt: Date.now(),
          audio_start_time: s.audio_start_time,
          audio_end_time: s.audio_end_time,
          duration: s.duration,
        }));
      }

      // Nothing on disk: this may be an auto-save-off recording.
      const stored = await indexedDBService.getTranscripts(meetingId);
      stored.sort((a, b) => (a.sequenceId || 0) - (b.sequenceId || 0));
      return stored;
    } catch (error) {
      console.error('Failed to load meeting transcripts:', error);
      return [];
    }
  }, []);

  /**
   * Recover a meeting: merge its audio checkpoints, attach the transcripts it
   * left behind and complete it.
   */
  const recoverMeeting = useCallback(async (meetingId: string): Promise<RecoveryOutcome> => {
    setIsRecovering(true);
    try {
      const result = await invoke<RecoveryResult>('api_recover_meeting', { meetingId });

      // Recovery found nothing on disk: fall back to whatever the webview stored
      // for this same meeting id (auto-save-off recordings).
      if (result.transcript_count === 0) {
        const stored = await indexedDBService.getTranscripts(meetingId);
        if (stored.length > 0) {
          stored.sort((a, b) => (a.sequenceId || 0) - (b.sequenceId || 0));
          const { storageService } = await import('@/services/storageService');
          // Reuse the title the backend just finalized with: passing anything
          // else here would silently rename the meeting.
          await storageService.saveMeeting(
            result.title,
            stored.map((t, index) => ({
              id: t.id?.toString() || `${meetingId}-${index}`,
              text: t.text,
              timestamp: t.timestamp,
              sequence_id: t.sequenceId || index,
              is_partial: false,
              confidence: t.confidence,
              audio_start_time: (t as any).audio_start_time,
              audio_end_time: (t as any).audio_end_time,
              duration: (t as any).duration,
            })) as any,
            null,
            meetingId
          );
        }
      }

      await indexedDBService.markMeetingSaved(meetingId);

      setRecoverableMeetings(prev => prev.filter(m => m.meetingId !== meetingId));

      return {
        success: true,
        audioStatus: result.audio_status,
        meetingId: result.meeting_id,
      };
    } catch (error) {
      console.error('Failed to recover meeting:', error);
      throw error;
    } finally {
      setIsRecovering(false);
    }
  }, []);

  /**
   * Discard an interrupted recording.
   *
   * Drops the database row and the webview copy. Files the recording wrote stay
   * on disk, so nothing is destroyed beyond recovery.
   */
  const deleteRecoverableMeeting = useCallback(async (meetingId: string): Promise<void> => {
    try {
      await invoke('api_discard_incomplete_meeting', { meetingId });
      await indexedDBService.deleteMeeting(meetingId);
      setRecoverableMeetings(prev => prev.filter(m => m.meetingId !== meetingId));
    } catch (error) {
      console.error('Failed to discard meeting:', error);
      throw error;
    }
  }, []);

  return {
    recoverableMeetings,
    isLoading,
    isRecovering,
    checkForRecoverableTranscripts,
    recoverMeeting,
    loadMeetingTranscripts,
    deleteRecoverableMeeting
  };
}
