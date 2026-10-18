/**
 * Display helpers for transcript speaker labels.
 *
 * Stored values: "mic" (the local user), "system" (everyone on the system audio
 * channel), pyannote diarization labels ("SPEAKER_00") or any other label from
 * an imported diarization file.
 */

import { invoke } from '@tauri-apps/api/core';

/** Stored speaker value of the microphone channel (the local user) */
export const MIC_SPEAKER = 'mic';

const DIARIZATION_LABEL = /^SPEAKER_(\d+)$/;

/** Human readable speaker name, or null when the segment has no speaker */
export function formatSpeakerLabel(speaker?: string | null): string | null {
  if (!speaker) return null;
  if (speaker === 'mic') return 'Me';
  if (speaker === 'system') return 'Others';
  const match = DIARIZATION_LABEL.exec(speaker);
  if (match) return `Speaker ${parseInt(match[1], 10) + 1}`;
  return speaker;
}

/**
 * "[Name] " prefix for plain text exports (copy, summary), empty when no speaker.
 * With userName, the local user is named ("[Me (Lucas)]") so an LLM knows who "Me" is.
 */
export function speakerPrefix(speaker?: string | null, userName?: string | null): string {
  const label = speaker === 'mic' && userName ? `Me (${userName})` : formatSpeakerLabel(speaker);
  return label ? `[${label}] ` : '';
}

let localUserNamePromise: Promise<string | null> | null = null;

/** Name of the local user (OS account), fetched once from the backend */
export function getLocalUserName(): Promise<string | null> {
  if (!localUserNamePromise) {
    localUserNamePromise = invoke<string | null>('get_local_user_name_command').catch(() => null);
  }
  return localUserNamePromise;
}

/**
 * Stable color slot for a speaker: 0 for the local user, 1..n for everyone else.
 * Callers map the slot to a color so each speaker keeps the same color.
 */
export function speakerColorIndex(speaker: string): number {
  if (speaker === 'mic') return 0;
  const match = DIARIZATION_LABEL.exec(speaker);
  if (match) return parseInt(match[1], 10) + 1;
  let hash = 0;
  for (let i = 0; i < speaker.length; i++) {
    hash = (hash * 31 + speaker.charCodeAt(i)) | 0;
  }
  return (Math.abs(hash) % 7) + 1;
}

const AUTOMATIC_LABEL = /^(mic|system|SPEAKER_\d+)$/;

/** True for labels set by the app (microphone, system audio, diarization), false for a given name */
export function isAutomaticSpeaker(speaker: string): boolean {
  return AUTOMATIC_LABEL.test(speaker);
}

/** One speaker of a meeting (audio/speaker_names.rs SpeakerInfo) */
export interface SpeakerInfo {
  speaker: string;
  label: string;
  segments: number;
  seconds: number;
  sample: string | null;
  named: boolean;
}

export interface SpeakerEvidenceLine {
  time: number | null;
  label: string | null;
  text: string;
}

/** A name the LLM proposes for a speaker, with the transcript lines that show it */
export interface SpeakerNameSuggestion {
  speaker: string;
  label: string;
  name: string;
  confidence: 'high' | 'medium' | 'low';
  reason: string;
  evidence: SpeakerEvidenceLine[];
}

export function getMeetingSpeakers(meetingId: string): Promise<SpeakerInfo[]> {
  return invoke<SpeakerInfo[]>('get_meeting_speakers_command', { meetingId });
}

/** Renames every line of `speaker`; resolves to the number of lines changed */
export function renameSpeaker(meetingId: string, speaker: string, name: string): Promise<number> {
  return invoke<number>('rename_speaker_command', { meetingId, speaker, name });
}

export function suggestSpeakerNames(meetingId: string): Promise<SpeakerNameSuggestion[]> {
  return invoke<SpeakerNameSuggestion[]>('suggest_speaker_names_command', { meetingId });
}

/** Error from a Tauri invoke as a readable message */
export function invokeErrorMessage(err: unknown): string {
  if (typeof err === 'string') return err;
  if (err instanceof Error) return err.message;
  return String(err);
}
