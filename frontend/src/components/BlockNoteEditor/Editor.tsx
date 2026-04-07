"use client";

import { useEffect, useState } from "react";
import { PartialBlock, Block } from "@blocknote/core";
import "@blocknote/shadcn/style.css";
import "@blocknote/core/fonts/inter.css";

interface EditorProps {
  initialContent?: Block[];
  onChange?: (blocks: Block[]) => void;
  editable?: boolean;
}

// Validate that blocks have the minimum required structure for BlockNote/ProseMirror
function sanitizeBlocks(blocks: Block[] | undefined): PartialBlock[] | undefined {
  if (!blocks || !Array.isArray(blocks) || blocks.length === 0) {
    return undefined;
  }

  try {
    const valid = blocks.filter(
      (block): block is Block =>
        !!block && typeof block === 'object' && !!block.type && typeof block.type === 'string'
    );
    return valid.length > 0 ? (valid as PartialBlock[]) : undefined;
  } catch (err) {
    console.error('❌ EDITOR: Failed to sanitize blocks, using empty editor:', err);
    return undefined;
  }
}

export default function Editor({ initialContent, onChange, editable = true }: EditorProps) {
  const [hasError, setHasError] = useState(false);

  // Lazy import to avoid SSR issues
  const { useCreateBlockNote } = require("@blocknote/react");
  const { BlockNoteView } = require("@blocknote/shadcn");

  const sanitizedContent = sanitizeBlocks(initialContent);

  const editor = useCreateBlockNote({
    initialContent: sanitizedContent,
  });

  // Handle content changes
  useEffect(() => {
    if (!onChange) return;

    const handleChange = () => {
      onChange(editor.document);
    };

    const unsubscribe = editor.onChange(handleChange);

    return () => {
      if (typeof unsubscribe === 'function') {
        unsubscribe();
      }
    };
  }, [editor, onChange]);

  if (hasError) {
    return (
      <div className="p-4 text-sm text-red-600 bg-red-50 rounded-md">
        Failed to render summary editor. The summary data may be corrupted.
      </div>
    );
  }

  return <BlockNoteView editor={editor} editable={editable} theme="light" />;
}
