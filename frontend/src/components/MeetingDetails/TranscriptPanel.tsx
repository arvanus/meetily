"use client";

import { Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { ContextAttachmentsBar } from './ContextAttachmentsBar';
import type { ContextAttachment } from '@/hooks/meeting-details/useSummaryContext';
import { useEffect, useMemo, useRef, useState } from 'react';
import { getCurrentWebview } from '@tauri-apps/api/webview';

interface TranscriptPanelProps {
  transcripts: Transcript[];
  contextPrompt: string;
  onContextPromptChange: (value: string) => void;
  attachments: ContextAttachment[];
  onPickAttachment: () => Promise<void>;
  onAddAttachment: (sourcePath: string) => Promise<void>;
  onRemoveAttachment: (id: string) => Promise<void>;
  onOpenAttachment: (id: string) => Promise<void>;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  isRecording: boolean;
  disableAutoScroll?: boolean;

  // Optional pagination props (when using virtualization)
  usePagination?: boolean;
  segments?: TranscriptSegmentData[];
  hasMore?: boolean;
  isLoadingMore?: boolean;
  totalCount?: number;
  loadedCount?: number;
  onLoadMore?: () => void;

  // Retranscription props
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}

export function TranscriptPanel({
  transcripts,
  contextPrompt,
  onContextPromptChange,
  attachments,
  onPickAttachment,
  onAddAttachment,
  onRemoveAttachment,
  onOpenAttachment,
  onCopyTranscript,
  onOpenMeetingFolder,
  isRecording,
  disableAutoScroll = false,
  usePagination = false,
  segments,
  hasMore,
  isLoadingMore,
  totalCount,
  loadedCount,
  onLoadMore,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptPanelProps) {
  const convertedSegments = useMemo(() => {
    if (usePagination && segments) {
      return segments;
    }
    return transcripts.map(t => ({
      id: t.id,
      timestamp: t.audio_start_time ?? 0,
      endTime: t.audio_end_time,
      text: t.text,
      confidence: t.confidence,
    }));
  }, [transcripts, usePagination, segments]);

  // Native (Tauri) drag-and-drop for adding attachments.
  const dropRef = useRef<HTMLDivElement>(null);
  const [dragOver, setDragOver] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      try {
        const webview = getCurrentWebview();
        const handle = await webview.onDragDropEvent((event) => {
          const el = dropRef.current;
          if (!el) return;
          const rect = el.getBoundingClientRect();
          const pos = (event.payload as { position?: { x: number; y: number } }).position;
          const inside =
            !!pos &&
            pos.x >= rect.left &&
            pos.x <= rect.right &&
            pos.y >= rect.top &&
            pos.y <= rect.bottom;

          if (event.payload.type === 'over') {
            setDragOver(inside);
          } else if (event.payload.type === 'drop') {
            setDragOver(false);
            const paths = (event.payload as { paths?: string[] }).paths;
            if (inside && paths && paths.length > 0) {
              for (const p of paths) {
                void onAddAttachment(p);
              }
            }
          } else if (event.payload.type === 'leave') {
            setDragOver(false);
          }
        });
        if (cancelled) {
          handle();
        } else {
          unlisten = handle;
        }
      } catch (e) {
        console.warn('Failed to install drag-drop listener:', e);
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [onAddAttachment]);

  return (
    <div className="hidden md:flex md:w-1/4 lg:w-1/3 min-w-0 border-r border-gray-200 bg-white flex-col relative shrink-0">
      {/* Title area */}
      <div className="p-4 border-b border-gray-200">
        <TranscriptButtonGroup
          transcriptCount={usePagination ? (totalCount ?? convertedSegments.length) : (transcripts?.length || 0)}
          onCopyTranscript={onCopyTranscript}
          onOpenMeetingFolder={onOpenMeetingFolder}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onRefetchTranscripts={onRefetchTranscripts}
        />
      </div>

      {/* Transcript content - use virtualized view for better performance */}
      <div className="flex-1 overflow-hidden pb-4">
        <VirtualizedTranscriptView
          segments={convertedSegments}
          isRecording={isRecording}
          isPaused={false}
          isProcessing={false}
          isStopping={false}
          enableStreaming={false}
          showConfidence={true}
          disableAutoScroll={disableAutoScroll}
          hasMore={hasMore}
          isLoadingMore={isLoadingMore}
          totalCount={totalCount}
          loadedCount={loadedCount}
          onLoadMore={onLoadMore}
        />
      </div>

      {/* Context attachments + textarea */}
      {!isRecording && convertedSegments.length > 0 && (
        <div
          ref={dropRef}
          className={dragOver ? 'ring-2 ring-blue-300 bg-blue-50/40 transition-colors' : 'transition-colors'}
        >
          <ContextAttachmentsBar
            attachments={attachments}
            onPick={onPickAttachment}
            onRemove={onRemoveAttachment}
            onOpen={onOpenAttachment}
          />
          <div className="p-1 border-t border-gray-200">
            <textarea
              placeholder={
                dragOver
                  ? 'Solte para anexar...'
                  : 'Add context for AI summary. For example people involved, meeting overview, objective etc...'
              }
              className="w-full px-3 py-2 border border-gray-200 rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 bg-white shadow-sm min-h-[80px] resize-y"
              value={contextPrompt}
              onChange={(e) => onContextPromptChange(e.target.value)}
            />
          </div>
        </div>
      )}
    </div>
  );
}
