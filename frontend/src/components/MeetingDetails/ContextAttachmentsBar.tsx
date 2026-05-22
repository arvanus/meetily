"use client";

import { Paperclip, X, AlertTriangle } from 'lucide-react';
import type { ContextAttachment } from '@/hooks/meeting-details/useSummaryContext';

interface ContextAttachmentsBarProps {
  attachments: ContextAttachment[];
  onPick: () => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onOpen: (id: string) => Promise<void>;
  disabled?: boolean;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export function ContextAttachmentsBar({
  attachments,
  onPick,
  onRemove,
  onOpen,
  disabled = false,
}: ContextAttachmentsBarProps) {
  const maxReached = attachments.length >= 10;

  return (
    <div className="px-1 pt-2 pb-1 border-t border-gray-200">
      {attachments.length > 0 && (
        <div className="flex flex-wrap gap-1.5 mb-2 px-2">
          {attachments.map((a) => (
            <div
              key={a.id}
              className="inline-flex items-center gap-1.5 px-2 py-1 bg-gray-100 hover:bg-gray-200 rounded-md text-xs transition-colors"
              title={`${a.display_name} — ${formatBytes(a.size_bytes)}${
                a.truncated ? ' (truncado em 256 KB)' : ''
              }`}
            >
              <Paperclip size={12} className="text-gray-600" />
              <button
                type="button"
                onClick={() => onOpen(a.id)}
                className="max-w-[160px] truncate text-left hover:underline"
              >
                {a.display_name}
              </button>
              {a.truncated && (
                <AlertTriangle size={12} className="text-amber-500" />
              )}
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onRemove(a.id);
                }}
                className="text-gray-500 hover:text-red-600"
                aria-label="Remover anexo"
              >
                <X size={12} />
              </button>
            </div>
          ))}
        </div>
      )}
      <div className="flex justify-end px-2">
        <button
          type="button"
          onClick={onPick}
          disabled={disabled || maxReached}
          className="inline-flex items-center gap-1 px-2 py-1 text-xs text-gray-600 hover:text-gray-900 disabled:opacity-50 disabled:cursor-not-allowed"
          title={
            maxReached
              ? 'Limite de 10 anexos atingido'
              : 'Anexar arquivo de texto'
          }
        >
          <Paperclip size={14} />
          Anexar
        </button>
      </div>
    </div>
  );
}
