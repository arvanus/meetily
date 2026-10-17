import React, { useState, useEffect, useRef } from 'react';
import { Users, Loader2, AlertCircle, X, FileUp, Play } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';
import { formatSpeakerLabel } from '@/lib/speaker';

interface DiarizationDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  meetingFolderPath: string | null;
  onComplete?: () => void;
}

interface DiarizationImportResult {
  file_path: string;
  total_segments: number;
  updated_segments: number;
  speakers: string[];
}

interface DiarizationProgress {
  meeting_id: string;
  progress_percentage: number;
  message: string;
}

interface DiarizationComplete {
  meeting_id: string;
  result: DiarizationImportResult;
}

interface DiarizationError {
  meeting_id: string;
  error: string;
}

const CANCELLED_ERROR = 'Diarization cancelled';

function showResultToast(result: DiarizationImportResult) {
  const speakers = result.speakers.map(s => formatSpeakerLabel(s) ?? s).join(', ');
  toast.success(
    `Speakers imported: ${result.updated_segments} of ${result.total_segments} segments updated`,
    { description: speakers || undefined }
  );
}

export function DiarizationDialog({
  open,
  onOpenChange,
  meetingId,
  meetingFolderPath,
  onComplete,
}: DiarizationDialogProps) {
  const [numSpeakers, setNumSpeakers] = useState('');
  const [isMaxSpeakers, setIsMaxSpeakers] = useState(false);
  const [isProcessing, setIsProcessing] = useState(false);
  const [isImporting, setIsImporting] = useState(false);
  const [progress, setProgress] = useState<DiarizationProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Keep callbacks in refs so event listeners always see the latest ones
  const onCompleteRef = useRef(onComplete);
  const onOpenChangeRef = useRef(onOpenChange);
  useEffect(() => {
    onCompleteRef.current = onComplete;
    onOpenChangeRef.current = onOpenChange;
  }, [onComplete, onOpenChange]);

  // Reset state when the dialog opens (unless a run is still going)
  const prevOpenRef = useRef(false);
  useEffect(() => {
    const wasOpen = prevOpenRef.current;
    prevOpenRef.current = open;
    if (open && !wasOpen) {
      setError(null);
      invoke<boolean>('is_diarization_in_progress_command')
        .then(running => {
          setIsProcessing(running);
          if (!running) setProgress(null);
        })
        .catch(() => setIsProcessing(false));
    }
  }, [open]);

  // Listen for diarization events
  useEffect(() => {
    if (!open) return;

    const unlisteners: UnlistenFn[] = [];
    let cleanedUp = false;

    const setupListeners = async () => {
      const handlers = [
        listen<DiarizationProgress>('diarization-progress', (event) => {
          if (event.payload.meeting_id === meetingId) {
            setIsProcessing(true);
            setProgress(event.payload);
          }
        }),
        listen<DiarizationComplete>('diarization-complete', (event) => {
          if (event.payload.meeting_id !== meetingId) return;
          setIsProcessing(false);
          setProgress(null);
          showResultToast(event.payload.result);
          Analytics.track('diarization_completed', {
            speakers: event.payload.result.speakers.length.toString(),
            updated_segments: event.payload.result.updated_segments.toString(),
          });
          onCompleteRef.current?.();
          onOpenChangeRef.current(false);
        }),
        listen<DiarizationError>('diarization-error', (event) => {
          if (event.payload.meeting_id !== meetingId) return;
          setIsProcessing(false);
          setProgress(null);
          if (event.payload.error === CANCELLED_ERROR) {
            toast.info(CANCELLED_ERROR);
            return;
          }
          setError(event.payload.error);
          Analytics.trackError('diarization_failed', event.payload.error);
        }),
      ];

      for (const handler of handlers) {
        const unlisten = await handler;
        if (cleanedUp) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      }
    };

    setupListeners();

    return () => {
      cleanedUp = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [open, meetingId]);

  const parsedSpeakers = (): number | null => {
    const value = parseInt(numSpeakers, 10);
    return Number.isFinite(value) && value > 0 ? value : null;
  };

  const handleRun = async () => {
    if (!meetingFolderPath) {
      setError('Meeting folder path not available');
      return;
    }

    setIsProcessing(true);
    setError(null);
    setProgress(null);

    try {
      await Analytics.track('diarization_started', {
        num_speakers: numSpeakers || 'auto',
        speakers_mode: isMaxSpeakers ? 'max' : 'exact',
      });
      const speakers = parsedSpeakers();
      await invoke('start_diarization_command', {
        meetingId,
        meetingFolderPath,
        numSpeakers: isMaxSpeakers ? null : speakers,
        maxSpeakers: isMaxSpeakers ? speakers : null,
      });
    } catch (err: any) {
      setIsProcessing(false);
      setError(typeof err === 'string' ? err : (err?.message || String(err)));
    }
  };

  const handleImportFile = async () => {
    setIsImporting(true);
    setError(null);
    try {
      const result = await invoke<DiarizationImportResult | null>('import_diarization_command', {
        meetingId,
        meetingFolderPath,
      });
      if (!result) return;
      showResultToast(result);
      onComplete?.();
      onOpenChange(false);
    } catch (err: any) {
      setError(typeof err === 'string' ? err : (err?.message || String(err)));
    } finally {
      setIsImporting(false);
    }
  };

  const handleCancel = async () => {
    try {
      await invoke('cancel_diarization_command');
    } catch (err) {
      console.error('Failed to cancel diarization:', err);
    }
  };

  // Prevent closing during processing
  const handleOpenChange = (newOpen: boolean) => {
    if (!newOpen && isProcessing) {
      return;
    }
    onOpenChange(newOpen);
  };

  const blockWhileProcessing = (event: Event) => {
    if (isProcessing) {
      event.preventDefault();
    }
  };

  const busy = isProcessing || isImporting;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="sm:max-w-[450px]"
        onEscapeKeyDown={blockWhileProcessing}
        onInteractOutside={blockWhileProcessing}
      >
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {isProcessing ? (
              <>
                <Loader2 className="h-5 w-5 animate-spin text-blue-600" />
                Identifying speakers...
              </>
            ) : error ? (
              <>
                <AlertCircle className="h-5 w-5 text-red-600" />
                Diarization Failed
              </>
            ) : (
              <>
                <Users className="h-5 w-5 text-blue-600" />
                Identify Speakers
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {isProcessing
              ? progress?.message || 'Starting...'
              : error
                ? 'An error occurred while identifying speakers'
                : 'Label who said what in this transcript. The text is kept as is.'}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {!isProcessing && !error && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Users className="h-4 w-4 text-muted-foreground" />
                <span className="text-sm font-medium">Number of speakers</span>
              </div>
              <div className="flex items-center gap-2">
                <Input
                  type="number"
                  min={1}
                  max={50}
                  placeholder="Auto-detect"
                  value={numSpeakers}
                  onChange={(e) => setNumSpeakers(e.target.value)}
                  className="flex-1"
                />
                <label className="flex items-center gap-1.5 text-sm text-muted-foreground whitespace-nowrap">
                  <input
                    type="checkbox"
                    checked={isMaxSpeakers}
                    onChange={(e) => setIsMaxSpeakers(e.target.checked)}
                  />
                  At most
                </label>
              </div>
              <p className="text-xs text-muted-foreground">
                Optional, but improves accuracy a lot: without it, long meetings tend to
                split one person into several speakers. Check "At most" when you only know
                an upper bound. For recordings with separate audio tracks, count only the
                other participants (not you).
              </p>
            </div>
          )}

          {isProcessing && (
            <div className="space-y-2">
              <div className="w-full bg-gray-200 rounded-full h-3">
                <div
                  className="bg-blue-600 h-3 rounded-full transition-all duration-300 ease-out"
                  style={{ width: `${Math.min(progress?.progress_percentage ?? 0, 100)}%` }}
                />
              </div>
              <div className="flex justify-end text-xs text-gray-600">
                <span>{Math.round(progress?.progress_percentage ?? 0)}%</span>
              </div>
            </div>
          )}

          {error && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3 max-h-48 overflow-auto">
              <p className="text-sm text-red-800 whitespace-pre-wrap break-words">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter className="gap-2 sm:gap-0">
          {!isProcessing && !error && (
            <>
              <Button
                variant="outline"
                onClick={handleImportFile}
                disabled={busy}
                title="Import a diarization file (.json or .rttm) produced outside the app"
              >
                {isImporting ? (
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                ) : (
                  <FileUp className="h-4 w-4 mr-2" />
                )}
                Import File...
              </Button>
              <Button
                onClick={handleRun}
                className="bg-blue-600 hover:bg-blue-700"
                disabled={busy || !meetingFolderPath}
              >
                <Play className="h-4 w-4 mr-2" />
                Run Diarization
              </Button>
            </>
          )}
          {isProcessing && (
            <Button variant="outline" onClick={handleCancel}>
              <X className="h-4 w-4 mr-2" />
              Cancel
            </Button>
          )}
          {error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Close
              </Button>
              <Button variant="outline" onClick={() => setError(null)}>
                Try Again
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
