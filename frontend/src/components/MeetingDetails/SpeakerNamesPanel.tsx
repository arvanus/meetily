"use client";

import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, Loader2, Pencil, Sparkles, X } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import {
  getLocalUserName,
  getMeetingSpeakers,
  invokeErrorMessage,
  MIC_SPEAKER,
  renameSpeaker,
  SpeakerInfo,
  SpeakerNameSuggestion,
  suggestSpeakerNames,
} from '@/lib/speaker';

interface SpeakerNamesPanelProps {
  meetingId: string;
  /** Called after a rename, so the transcript can be reloaded */
  onRenamed?: () => void | Promise<void>;
  disabled?: boolean;
}

function formatDuration(seconds: number): string {
  const total = Math.round(seconds);
  return `${Math.floor(total / 60)}:${(total % 60).toString().padStart(2, '0')}`;
}

function formatTime(seconds: number | null): string {
  if (seconds === null) return '';
  const total = Math.floor(seconds);
  return `${Math.floor(total / 60).toString().padStart(2, '0')}:${(total % 60).toString().padStart(2, '0')}`;
}

const CONFIDENCE_STYLE: Record<SpeakerNameSuggestion['confidence'], string> = {
  high: 'text-emerald-700',
  medium: 'text-amber-700',
  low: 'text-red-700',
};

/** Speakers of a meeting with editable names, plus name suggestions from the summary LLM */
export function SpeakerNamesPanel({ meetingId, onRenamed, disabled }: SpeakerNamesPanelProps) {
  const [speakers, setSpeakers] = useState<SpeakerInfo[]>([]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [userName, setUserName] = useState<string | null>(null);
  const [saving, setSaving] = useState<string | null>(null);
  const [suggesting, setSuggesting] = useState(false);
  const [suggestions, setSuggestions] = useState<SpeakerNameSuggestion[] | null>(null);
  const inputRefs = useRef<Record<string, HTMLInputElement | null>>({});

  const load = useCallback(async () => {
    try {
      setSpeakers(await getMeetingSpeakers(meetingId));
    } catch (err) {
      console.error('Failed to load speakers:', err);
      setSpeakers([]);
    }
  }, [meetingId]);

  useEffect(() => {
    void load();
    getLocalUserName().then(setUserName);
    setSuggestions(null);
    setDrafts({});
  }, [load]);

  const rename = async (speaker: SpeakerInfo, rawName: string): Promise<boolean> => {
    const name = rawName.trim();
    if (!name || name === speaker.label) return false;
    setSaving(speaker.speaker);
    try {
      const changed = await renameSpeaker(meetingId, speaker.speaker, name);
      toast.success(`${speaker.label} renamed to ${name}`, { description: `${changed} lines updated` });
      setDrafts(d => {
        const { [speaker.speaker]: _, ...rest } = d;
        return rest;
      });
      setSuggestions(s => s?.filter(x => x.speaker !== speaker.speaker) ?? null);
      await load();
      await onRenamed?.();
      return true;
    } catch (err) {
      toast.error('Could not rename the speaker', { description: invokeErrorMessage(err) });
      return false;
    } finally {
      setSaving(null);
    }
  };

  const handleSuggest = async () => {
    setSuggesting(true);
    try {
      const result = await suggestSpeakerNames(meetingId);
      setSuggestions(result);
      if (result.length === 0) {
        toast.info('No speaker could be identified from the transcript');
      }
    } catch (err) {
      toast.error('Could not get name suggestions', { description: invokeErrorMessage(err) });
    } finally {
      setSuggesting(false);
    }
  };

  const findSpeaker = (key: string) => speakers.find(s => s.speaker === key);

  const acceptSuggestion = (suggestion: SpeakerNameSuggestion) => {
    const speaker = findSpeaker(suggestion.speaker);
    if (speaker) void rename(speaker, suggestion.name);
  };

  const editSuggestion = (suggestion: SpeakerNameSuggestion) => {
    setDrafts(d => ({ ...d, [suggestion.speaker]: suggestion.name }));
    setSuggestions(s => s?.filter(x => x !== suggestion) ?? null);
    setTimeout(() => inputRefs.current[suggestion.speaker]?.focus(), 0);
  };

  const ignoreSuggestion = (suggestion: SpeakerNameSuggestion) => {
    setSuggestions(s => s?.filter(x => x !== suggestion) ?? null);
  };

  if (speakers.length === 0) return null;

  const busy = disabled || saving !== null || suggesting;
  const hasUnnamed = speakers.some(s => !s.named && s.speaker !== MIC_SPEAKER);

  return (
    <div className="space-y-3 pb-4 border-b">
      <div className="flex items-center justify-between">
        <span className="text-sm font-medium">Speaker names</span>
        <Button
          variant="outline"
          size="sm"
          onClick={handleSuggest}
          disabled={busy || !hasUnnamed}
          title={hasUnnamed ? 'Ask the summary model who the unnamed speakers are' : 'Every speaker already has a name'}
        >
          {suggesting ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : <Sparkles className="h-4 w-4 mr-2" />}
          Suggest with AI
        </Button>
      </div>

      <div className="space-y-2 max-h-56 overflow-auto pr-1">
        {speakers.map(speaker => {
          const isMic = speaker.speaker === MIC_SPEAKER;
          const draft = drafts[speaker.speaker] ?? (speaker.named ? speaker.label : '');
          const changed = draft.trim() !== '' && draft.trim() !== (speaker.named ? speaker.label : '');
          return (
            <div key={speaker.speaker} className="flex items-center gap-2">
              <div className="w-28 shrink-0">
                <div className="text-xs font-semibold truncate" title={speaker.label}>
                  {isMic ? 'Me' : speaker.label}
                </div>
                <div className="text-[11px] text-muted-foreground">
                  {formatDuration(speaker.seconds)} · {speaker.segments} lines
                </div>
              </div>
              {isMic ? (
                <div className="flex-1 text-sm text-muted-foreground px-3">
                  {userName ? `Me (${userName})` : 'Me'}
                </div>
              ) : (
                <form
                  className="flex flex-1 gap-1"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void rename(speaker, draft);
                  }}
                >
                  <Input
                    ref={el => { inputRefs.current[speaker.speaker] = el; }}
                    value={draft}
                    placeholder="No name"
                    title={speaker.sample ?? undefined}
                    onChange={(e) => setDrafts(d => ({ ...d, [speaker.speaker]: e.target.value }))}
                    disabled={busy}
                    className="h-8"
                  />
                  <Button
                    type="submit"
                    size="sm"
                    variant="outline"
                    className="h-8 px-2"
                    disabled={busy || !changed}
                    title="Save name"
                  >
                    {saving === speaker.speaker ? <Loader2 className="h-4 w-4 animate-spin" /> : <Check className="h-4 w-4" />}
                  </Button>
                </form>
              )}
            </div>
          );
        })}
      </div>

      {suggestions && suggestions.length > 0 && (
        <div className="space-y-2">
          <span className="text-xs font-medium text-muted-foreground">Suggestions (check the lines before accepting)</span>
          {suggestions.map(suggestion => (
            <div key={`${suggestion.speaker}-${suggestion.name}`} className="border rounded-md p-2 text-sm">
              <div className="flex items-center gap-1">
                <span className="font-semibold">{suggestion.label}</span>
                <span>→</span>
                <span className="font-semibold">{suggestion.name}</span>
                <span className={`text-[11px] ml-1 ${CONFIDENCE_STYLE[suggestion.confidence] ?? ''}`}>
                  ({suggestion.confidence})
                </span>
                {!suggestion.verified && (
                  <span
                    className="text-[11px] ml-1 text-red-700"
                    title="The name is not said in these lines, or none of them is said by this speaker"
                  >
                    · no proof in the transcript
                  </span>
                )}
              </div>
              {suggestion.reason && <p className="text-xs text-muted-foreground mt-1">{suggestion.reason}</p>}
              <div className="mt-1 space-y-0.5 border-l-2 border-gray-200 pl-2">
                {suggestion.evidence.map((line, i) => (
                  <p key={i} className="text-xs text-gray-700">
                    <span className="text-gray-400">{formatTime(line.time)}</span>{' '}
                    {line.label && <span className="font-medium">{line.label}: </span>}
                    &quot;{line.text}&quot;
                  </p>
                ))}
              </div>
              <div className="flex gap-1 mt-2">
                <Button size="sm" className="h-7" disabled={busy} onClick={() => acceptSuggestion(suggestion)}>
                  <Check className="h-3 w-3 mr-1" /> Accept
                </Button>
                <Button size="sm" variant="outline" className="h-7" disabled={busy} onClick={() => editSuggestion(suggestion)}>
                  <Pencil className="h-3 w-3 mr-1" /> Edit
                </Button>
                <Button size="sm" variant="ghost" className="h-7" disabled={busy} onClick={() => ignoreSuggestion(suggestion)}>
                  <X className="h-3 w-3 mr-1" /> Ignore
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      <p className="text-xs text-muted-foreground">
        Names apply to the whole transcript; giving two speakers the same name merges them.
        Running diarization or re-transcribing again replaces them. Regenerate the summary
        to use the new names.
      </p>
    </div>
  );
}
