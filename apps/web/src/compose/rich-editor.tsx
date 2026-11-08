import { Button, cn, Tooltip, TooltipContent, TooltipTrigger } from '@holtmail/ui'
import { type Editor, EditorContent, useEditor, useEditorState } from '@tiptap/react'
import { StarterKit } from '@tiptap/starter-kit'
import {
  BoldIcon,
  ItalicIcon,
  LinkIcon,
  ListIcon,
  ListOrderedIcon,
  QuoteIcon,
  StrikethroughIcon,
} from 'lucide-react'
import { type ReactNode, useEffect } from 'react'

import { strings } from '../strings.ts'

const t = strings.writing.compose

function ToolButton({
  label,
  active,
  onClick,
  children,
}: {
  label: string
  active: boolean
  onClick: () => void
  children: ReactNode
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          aria-label={label}
          aria-pressed={active}
          className={cn(active && 'bg-bg-selected')}
          onMouseDown={(event) => event.preventDefault()}
          onClick={onClick}
        >
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}

function Toolbar({ editor }: { editor: Editor }) {
  const state = useEditorState({
    editor,
    selector: ({ editor: e }) => ({
      bold: e.isActive('bold'),
      italic: e.isActive('italic'),
      strike: e.isActive('strike'),
      bulletList: e.isActive('bulletList'),
      orderedList: e.isActive('orderedList'),
      blockquote: e.isActive('blockquote'),
      link: e.isActive('link'),
    }),
  })
  const chain = () => editor.chain().focus()
  return (
    <div role="toolbar" aria-label={t.toolbar} className="flex flex-wrap items-center gap-0.5">
      <ToolButton label={t.bold} active={state.bold} onClick={() => chain().toggleBold().run()}>
        <BoldIcon />
      </ToolButton>
      <ToolButton
        label={t.italic}
        active={state.italic}
        onClick={() => chain().toggleItalic().run()}
      >
        <ItalicIcon />
      </ToolButton>
      <ToolButton
        label={t.strike}
        active={state.strike}
        onClick={() => chain().toggleStrike().run()}
      >
        <StrikethroughIcon />
      </ToolButton>
      <ToolButton
        label={t.bulletList}
        active={state.bulletList}
        onClick={() => chain().toggleBulletList().run()}
      >
        <ListIcon />
      </ToolButton>
      <ToolButton
        label={t.orderedList}
        active={state.orderedList}
        onClick={() => chain().toggleOrderedList().run()}
      >
        <ListOrderedIcon />
      </ToolButton>
      <ToolButton
        label={t.quote}
        active={state.blockquote}
        onClick={() => chain().toggleBlockquote().run()}
      >
        <QuoteIcon />
      </ToolButton>
      <ToolButton
        label={t.link}
        active={state.link}
        onClick={() => {
          if (state.link) {
            chain().unsetLink().run()
            return
          }
          const href = window.prompt(t.linkPrompt, 'https://')?.trim()
          if (href && /^(https?:|mailto:)/i.test(href)) chain().setLink({ href }).run()
        }}
      >
        <LinkIcon />
      </ToolButton>
    </div>
  )
}

/** The rich text editor: paragraphs, bold, italic, strikethrough, lists, quotes and links. */
export function RichEditor({
  content,
  label,
  onChange,
  onReady,
  onSubmit,
}: {
  content: string
  label: string
  onChange: (html: string) => void
  onReady: (editor: Editor | null) => void
  onSubmit: () => void
}) {
  const editor = useEditor({
    extensions: [
      StarterKit.configure({
        heading: false,
        codeBlock: false,
        horizontalRule: false,
        link: { openOnClick: false, autolink: true, protocols: ['mailto'] },
      }),
    ],
    content,
    editorProps: {
      attributes: { 'aria-label': label, 'aria-multiline': 'true', role: 'textbox' },
      handleKeyDown: (_view, event) => {
        if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
          onSubmit()
          return true
        }
        return false
      },
    },
    onUpdate: ({ editor: e }) => onChange(e.getHTML()),
  })

  useEffect(() => {
    onReady(editor)
    return () => onReady(null)
  }, [editor, onReady])

  return (
    <div className="grid min-h-0 grid-rows-[auto_1fr] gap-2">
      {editor && <Toolbar editor={editor} />}
      <EditorContent
        editor={editor}
        className={cn(
          'min-h-48 text-sm text-fg',
          '[&_.ProseMirror]:min-h-48 [&_.ProseMirror]:outline-none',
          '[&_.ProseMirror_p]:my-2 [&_.ProseMirror_ul]:list-disc [&_.ProseMirror_ul]:pl-6',
          '[&_.ProseMirror_ol]:list-decimal [&_.ProseMirror_ol]:pl-6',
          '[&_.ProseMirror_blockquote]:border-l [&_.ProseMirror_blockquote]:border-border-strong [&_.ProseMirror_blockquote]:pl-3 [&_.ProseMirror_blockquote]:text-fg-muted',
          '[&_.ProseMirror_a]:text-accent [&_.ProseMirror_a]:underline',
        )}
      />
    </div>
  )
}
