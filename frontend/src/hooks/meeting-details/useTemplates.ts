import { useState, useEffect, useCallback } from 'react';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';

const DEFAULT_TEMPLATE = 'standard_meeting';

export function useTemplates(meetingId?: string) {
  const [availableTemplates, setAvailableTemplates] = useState<Array<{
    id: string;
    name: string;
    description: string;
  }>>([]);
  const [selectedTemplate, setSelectedTemplate] = useState<string>(DEFAULT_TEMPLATE);

  // Fetch available templates on mount
  useEffect(() => {
    const fetchTemplates = async () => {
      try {
        const templates = await invokeTauri('api_list_templates') as Array<{
          id: string;
          name: string;
          description: string;
        }>;
        console.log('Available templates:', templates);
        setAvailableTemplates(templates);
      } catch (error) {
        console.error('Failed to fetch templates:', error);
      }
    };
    fetchTemplates();
  }, []);

  // Restore the per-meeting template selection so it survives app restarts.
  useEffect(() => {
    if (!meetingId) return;
    let cancelled = false;
    invokeTauri('api_get_summary_context', { meetingId })
      .then((data) => {
        if (cancelled) return;
        const saved = (data as { template_id?: string | null })?.template_id;
        if (saved) {
          setSelectedTemplate(saved);
        }
      })
      .catch((error) => {
        console.error('Failed to load saved template:', error);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  // Handle template selection (and persist it per meeting)
  const handleTemplateSelection = useCallback((templateId: string, templateName: string) => {
    setSelectedTemplate(templateId);
    toast.success('Template selected', {
      description: `Using "${templateName}" template for summary generation`,
    });
    Analytics.trackFeatureUsed('template_selected');

    if (meetingId) {
      invokeTauri('api_save_summary_template', {
        meetingId,
        templateId,
      }).catch((error) => {
        console.error('Failed to persist template selection:', error);
      });
    }
  }, [meetingId]);

  return {
    availableTemplates,
    selectedTemplate,
    handleTemplateSelection,
  };
}
