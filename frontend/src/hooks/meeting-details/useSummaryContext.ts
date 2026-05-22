import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';

export interface ContextAttachment {
  id: string;
  display_name: string;
  size_bytes: number;
  truncated: boolean;
  created_at: string;
}

export interface SummaryContextData {
  context_prompt: string;
  attachments: ContextAttachment[];
}

export function useSummaryContext(meetingId: string | undefined) {
  const [contextPrompt, setContextPromptState] = useState('');
  const [attachments, setAttachments] = useState<ContextAttachment[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!meetingId) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    invoke<SummaryContextData>('api_get_summary_context', { meetingId })
      .then((data) => {
        if (cancelled) return;
        setContextPromptState(data.context_prompt ?? '');
        setAttachments(data.attachments ?? []);
        setError(null);
      })
      .catch((e) => {
        if (cancelled) return;
        console.error('Failed to load summary context:', e);
        setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  const setContextPrompt = useCallback(
    (value: string) => {
      setContextPromptState(value);
      if (!meetingId) return;
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        invoke('api_save_summary_context', {
          meetingId,
          contextPrompt: value,
        }).catch((e) => {
          console.error('Failed to save context:', e);
          toast.error('Falha ao salvar contexto', { description: String(e) });
        });
      }, 500);
    },
    [meetingId]
  );

  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, []);

  const addAttachment = useCallback(
    async (sourcePath: string) => {
      if (!meetingId) return;
      try {
        const info = await invoke<ContextAttachment>('api_add_context_attachment', {
          meetingId,
          sourcePath,
        });
        setAttachments((prev) => [...prev, info]);
        if (info.truncated) {
          toast.warning(`Arquivo "${info.display_name}" foi truncado em 256 KB`);
        }
      } catch (e) {
        toast.error('Falha ao anexar arquivo', { description: String(e) });
      }
    },
    [meetingId]
  );

  const pickAndAddAttachment = useCallback(async () => {
    if (!meetingId) return;
    try {
      const path = await invoke<string | null>('api_pick_context_attachment_file');
      if (typeof path === 'string' && path.length > 0) {
        await addAttachment(path);
      }
    } catch (e) {
      console.error('Failed to pick file:', e);
      toast.error('Falha ao abrir o seletor de arquivos', { description: String(e) });
    }
  }, [meetingId, addAttachment]);

  const removeAttachment = useCallback(
    async (attachmentId: string) => {
      if (!meetingId) return;
      try {
        await invoke('api_remove_context_attachment', { meetingId, attachmentId });
        setAttachments((prev) => prev.filter((a) => a.id !== attachmentId));
      } catch (e) {
        toast.error('Falha ao remover anexo', { description: String(e) });
      }
    },
    [meetingId]
  );

  const openAttachment = useCallback(
    async (attachmentId: string) => {
      if (!meetingId) return;
      try {
        await invoke('api_open_context_attachment', { meetingId, attachmentId });
      } catch (e) {
        toast.error('Falha ao abrir anexo', { description: String(e) });
      }
    },
    [meetingId]
  );

  return {
    contextPrompt,
    setContextPrompt,
    attachments,
    addAttachment,
    pickAndAddAttachment,
    removeAttachment,
    openAttachment,
    loading,
    error,
  };
}
