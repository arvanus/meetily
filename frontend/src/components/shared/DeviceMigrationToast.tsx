'use client';

import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { Mic, Volume2 } from 'lucide-react';

/**
 * Emitted by the Rust side when a recording that follows the system default
 * device migrates to a new one (headset plugged in, microphone unplugged).
 * Roles pinned to a specific device in preferences never emit this.
 */
interface DeviceMigration {
  role: 'microphone' | 'system';
  from: string;
  to: string;
}

/**
 * Tells the user their recording moved to another device, so a mid-meeting
 * device switch is never silent.
 */
export function useDeviceMigrationToast() {
  useEffect(() => {
    let cleanedUp = false;
    let unlisten: (() => void) | undefined;

    listen<DeviceMigration>('audio-device-migrated', (event) => {
      const { role, from, to } = event.payload;
      const isMicrophone = role === 'microphone';

      toast.info(isMicrophone ? 'Microphone switched' : 'System audio switched', {
        description: `Recording moved to "${to}" (was "${from}").`,
        icon: isMicrophone ? <Mic className="w-4 h-4" /> : <Volume2 className="w-4 h-4" />,
        duration: 6000,
      });
    })
      .then((fn) => {
        // The listener may resolve after unmount
        if (cleanedUp) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error) => {
        console.error('[DeviceMigrationToast] Failed to listen for device migrations:', error);
      });

    return () => {
      cleanedUp = true;
      unlisten?.();
    };
  }, []);
}

/** Mount once, app-wide. Renders nothing. */
export function DeviceMigrationToastProvider() {
  useDeviceMigrationToast();
  return null;
}
