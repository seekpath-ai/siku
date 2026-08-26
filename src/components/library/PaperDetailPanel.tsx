import { useState, useEffect, useMemo } from 'react';
import {
  Save,
  FileText,
  Tag,
  StickyNote,
  Plus,
  Trash2,
  ChevronDown,
  Info,
  ArrowLeft,
  Loader2,
  RefreshCw,
  Link2,
  X,
  Paperclip,
  FileDown,
} from 'lucide-react';
import { useLibraryStore } from '@/stores/libraryStore';
import { usePetContextStore } from '@/stores/petContextStore';
import { usePaper, useUpdatePaper, usePaperNotes, usePaperTags, useTags, useAddTagsToPaper, useRemoveTagsFromPaper, useCreateTag } from '@/hooks/useLibrary';
import { useDialog } from '@/hooks/useDialog';
import { useTabStore } from '@/stores/tabStore';
import { parseJsonArray } from '@/lib/types';
import { paperListRelated, paperAddRelated, paperRemoveRelated, listPapers, paperGetCollections, paperEnrichMetadata, paperGetCreators, paperSetCreators, paperListAttachments, paperAddAttachment, paperRemoveAttachment, paperOpenAttachment, paperExportAnnotations, notesListAll, notesUpdate, notesDelete, notesGetBacklinks, noteCreateUnderPaper, noteMergeIntoExcerpt, type Creator } from '@/lib/tauri';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { NoteEditor } from '@/components/notes/NoteEditor';
import type { Paper, Note } from '@/lib/types';

type TabKey = 'info' | 'notes' | 'tags';

function SidebarToggleIcon({ collapsed, side = 'left' }: { collapsed: boolean; side?: 'left' | 'right' }) {
  const isRight = side === 'right';
  return (
    <div className="relative w-4 h-4 border border-current rounded-[3px]">
      <div
        className={`absolute top-[3px] bottom-[3px] rounded-[2px] bg-current transition-all duration-200 ${
          isRight
            ? collapsed
              ? 'right-[3px] w-[1px]'
              : 'right-[3px] w-[3px]'
            : collapsed
              ? 'left-[3px] w-[1px]'
              : 'left-[3px] w-[3px]'
        }`}
      />
    </div>
  );
}

function Field({
  label,
  value,
  onChange,
  multiline,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  multiline?: boolean;
  placeholder?: string;
}) {
  return (
    <label className="block">
      <span className="text-[11px] font-medium text-text-secondary/70 uppercase tracking-wider">{label}</span>
      {multiline ? (
        <textarea
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          rows={4}
          className="mt-1 w-full rounded-lg bg-surface border border-surface-hover px-2.5 py-1.5 text-xs text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50 resize-none"
        />
      ) : (
        <input
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          className="mt-1 w-full h-8 rounded-lg bg-surface border border-surface-hover px-2.5 text-xs text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50"
        />
      )}
    </label>
  );
}

function SelectField({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (v: string) => void;
}) {
  return (
    <label className="block">
      <span className="text-[11px] font-medium text-text-secondary/70 uppercase tracking-wider">{label}</span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="mt-1 w-full h-8 rounded-lg bg-surface border border-surface-hover px-2 text-xs text-text-primary focus:outline-none focus:border-primary/50"
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}

const ITEM_TYPES: { value: string; label: string }[] = [
  { value: 'journal', label: '期刊文章' },
  { value: 'book', label: '书籍' },
  { value: 'bookSection', label: '书籍章节' },
  { value: 'conference', label: '会议论文' },
  { value: 'thesis', label: '学位论文' },
  { value: 'report', label: '报告' },
  { value: 'webpage', label: '网页' },
  { value: 'newspaper', label: '报纸文章' },
  { value: 'patent', label: '专利' },
  { value: 'other', label: '其他' },
];

/// Fields hidden per item type (Zotero-style per-type field sets). Keys match
/// the `form` fields rendered in the InfoTab grid.
const HIDDEN_FIELDS_BY_TYPE: Record<string, string[]> = {
  journal: [],
  book: ['volume', 'issue', 'journal', 'conferenceName', 'numPages'],
  bookSection: ['volume', 'issue', 'journal', 'conferenceName', 'numPages'],
  conference: ['journal', 'volume', 'issue', 'edition', 'isbn', 'issn', 'series'],
  thesis: ['journal', 'volume', 'issue', 'conferenceName', 'isbn', 'issn', 'series', 'edition'],
  report: ['volume', 'issue', 'conferenceName', 'isbn', 'issn', 'series', 'edition'],
  patent: ['journal', 'volume', 'issue', 'conferenceName', 'isbn', 'issn', 'series', 'edition', 'abstract'],
  webpage: ['journal', 'volume', 'issue', 'pages', 'conferenceName', 'publisher', 'place', 'series', 'edition', 'isbn', 'issn', 'numPages', 'archiveLocation', 'callNumber'],
  newspaper: ['journal', 'conferenceName', 'publisher', 'place', 'series', 'edition', 'isbn', 'issn', 'numPages', 'archiveLocation', 'callNumber'],
  other: [],
};

const CREATOR_ROLES = [
  { value: 'author', label: '作者' },
  { value: 'editor', label: '编者' },
  { value: 'translator', label: '译者' },
];

/** Structured creator editor (role + name rows). */
function CreatorEditor({
  creators,
  onChange,
}: {
  creators: Creator[];
  onChange: (cs: Creator[]) => void;
}) {
  const update = (i: number, patch: Partial<Creator>) =>
    onChange(creators.map((c, idx) => (idx === i ? { ...c, ...patch } : c)));
  const display = (c: Creator) =>
    c.name || [c.first_name, c.last_name].filter(Boolean).join(' ');
  return (
    <div className="space-y-1.5">
      {creators.map((c, i) => (
        <div key={i} className="flex items-center gap-1.5">
          <select
            value={c.role}
            onChange={(e) => update(i, { role: e.target.value })}
            className="w-16 h-7 shrink-0 rounded-lg bg-surface border border-surface-hover text-xs text-text-primary focus:outline-none focus:border-primary/50"
          >
            {CREATOR_ROLES.map((r) => (
              <option key={r.value} value={r.value}>
                {r.label}
              </option>
            ))}
          </select>
          <input
            type="text"
            value={display(c)}
            onChange={(e) => update(i, { name: e.target.value })}
            placeholder="姓名（如：Einstein, Albert 或 王小明）"
            className="flex-1 min-w-0 h-7 px-2 rounded-lg bg-surface border border-surface-hover text-xs text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/50"
          />
          <button
            onClick={() => onChange(creators.filter((_, idx) => idx !== i))}
            className="shrink-0 p-0.5 rounded text-text-secondary/50 hover:text-red-400"
            title="移除"
          >
            <X size={12} />
          </button>
        </div>
      ))}
      <button
        onClick={() =>
          onChange([...creators, { role: 'author', last_name: '', first_name: '', name: '' }])
        }
        className="flex items-center gap-1 text-xs text-text-secondary hover:text-primary transition-colors"
      >
        <Plus size={12} /> 添加创作者
      </button>
    </div>
  );
}

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString('zh-CN', {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return iso;
  }
}

function InfoTab({ paper }: { paper: Paper }) {
  const updateMutation = useUpdatePaper();
  const queryClient = useQueryClient();
  const { alert, select } = useDialog();
  const [enriching, setEnriching] = useState(false);
  const [collections, setCollections] = useState<{ id: string; name: string }[]>([]);

  // Related papers (bidirectional links).
  const { data: related = [], isLoading: relatedLoading } = useQuery({
    queryKey: ['paper-related', paper.id],
    queryFn: () => paperListRelated(paper.id),
  });
  const handleAddRelated = async () => {
    try {
      const all = await listPapers({ limit: 200, sort_by: 'title', sort_order: 'asc' });
      const candidates = all.filter(
        (p) => p.id !== paper.id && !related.some((r) => r.id === p.id)
      );
      if (candidates.length === 0) {
        await alert('没有可关联的文献', '添加关联');
        return;
      }
      const choice = await select('选择要关联的文献：', {
        title: '添加关联',
        options: candidates.map((p) => ({
          label: `${p.title}${p.year ? ` (${p.year})` : ''}`,
          value: p.id,
        })),
      });
      if (!choice) return;
      await paperAddRelated(paper.id, choice);
      queryClient.invalidateQueries({ queryKey: ['paper-related', paper.id] });
    } catch (err) {
      await alert(`添加关联失败: ${err}`, '添加关联');
    }
  };
  const handleRemoveRelated = async (relatedId: string) => {
    try {
      await paperRemoveRelated(paper.id, relatedId);
      queryClient.invalidateQueries({ queryKey: ['paper-related', paper.id] });
    } catch (err) {
      await alert(`取消关联失败: ${err}`);
    }
  };

  // Attachments (multi-file per paper).
  const { data: attachments = [], isLoading: attachmentsLoading } = useQuery({
    queryKey: ['paper-attachments', paper.id],
    queryFn: () => paperListAttachments(paper.id),
  });
  const handleAddAttachment = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        multiple: false,
        filters: [{ name: '文件', extensions: ['pdf', 'png', 'jpg', 'jpeg', 'txt', 'md', 'docx', 'epub', 'zip'] }],
      });
      if (!selected || Array.isArray(selected)) return;
      await paperAddAttachment(paper.id, selected);
      queryClient.invalidateQueries({ queryKey: ['paper-attachments', paper.id] });
    } catch (err) {
      await alert(`添加附件失败: ${err}`, '添加附件');
    }
  };
  const handleRemoveAttachment = async (attachmentId: string) => {
    try {
      await paperRemoveAttachment(attachmentId);
      queryClient.invalidateQueries({ queryKey: ['paper-attachments', paper.id] });
    } catch (err) {
      await alert(`删除附件失败: ${err}`);
    }
  };
  const handleExportAnnotations = async () => {
    try {
      const md = await paperExportAnnotations(paper.id);
      await navigator.clipboard.writeText(md);
      await alert('已复制标注（Markdown）到剪贴板', '导出标注');
    } catch (err) {
      await alert(`导出标注失败: ${err}`, '导出标注');
    }
  };

  // Zotero-style: resolve DOI / title via CrossRef and fill blank metadata.
  const handleEnrich = async () => {
    if (enriching) return;
    setEnriching(true);
    try {
      const filled = await paperEnrichMetadata(paper.id);
      await alert(
        filled ? '已根据 CrossRef 补全文献元数据。' : '未找到可补全的元数据（可能缺少 DOI 或标题未匹配）。',
        '补充元数据'
      );
      queryClient.invalidateQueries({ queryKey: ['paper', paper.id] });
      queryClient.invalidateQueries({ queryKey: ['papers'] });
    } catch (err) {
      await alert(`补充元数据失败：${err}`, '补充元数据');
    } finally {
      setEnriching(false);
    }
  };
  const [form, setForm] = useState({
    title: paper.title,
    authors: parseJsonArray(paper.authors).join('; '),
    year: paper.year?.toString() ?? '',
    journal: paper.journal ?? '',
    doi: paper.doi ?? '',
    url: paper.url ?? '',
    abstract: paper.abstract_text ?? '',
    keywords: parseJsonArray(paper.keywords).join(', '),
    citationKey: paper.citation_key ?? '',
    itemType: paper.item_type ?? 'journal',
    volume: paper.volume ?? '',
    issue: paper.issue ?? '',
    pages: paper.pages ?? '',
    conferenceName: paper.conference_name ?? '',
    publisher: paper.publisher ?? '',
    place: paper.place ?? '',
    editor: parseJsonArray(paper.editor).join('; '),
    series: paper.series ?? '',
    edition: paper.edition ?? '',
    isbn: paper.isbn ?? '',
    issn: paper.issn ?? '',
    language: paper.language ?? '',
    numPages: paper.num_pages?.toString() ?? paper.page_count?.toString() ?? '',
    archiveLocation: paper.archive_location ?? '',
    callNumber: paper.call_number ?? '',
    rights: paper.rights ?? '',
  });

  useEffect(() => {
    setForm({
      title: paper.title,
      authors: parseJsonArray(paper.authors).join('; '),
      year: paper.year?.toString() ?? '',
      journal: paper.journal ?? '',
      doi: paper.doi ?? '',
      url: paper.url ?? '',
      abstract: paper.abstract_text ?? '',
      keywords: parseJsonArray(paper.keywords).join(', '),
      citationKey: paper.citation_key ?? '',
      itemType: paper.item_type ?? 'journal',
      volume: paper.volume ?? '',
      issue: paper.issue ?? '',
      pages: paper.pages ?? '',
      conferenceName: paper.conference_name ?? '',
      publisher: paper.publisher ?? '',
      place: paper.place ?? '',
      editor: parseJsonArray(paper.editor).join('; '),
      series: paper.series ?? '',
      edition: paper.edition ?? '',
      isbn: paper.isbn ?? '',
      issn: paper.issn ?? '',
      language: paper.language ?? '',
      numPages: paper.num_pages?.toString() ?? paper.page_count?.toString() ?? '',
      archiveLocation: paper.archive_location ?? '',
      callNumber: paper.call_number ?? '',
      rights: paper.rights ?? '',
    });
  }, [paper]);

  // Structured creators: load once per paper; keep the flat authors/editor
  // form fields (the sync transport) derived from them.
  const [creators, setCreators] = useState<Creator[]>([]);
  const [creatorsLoaded, setCreatorsLoaded] = useState(false);
  useEffect(() => {
    let cancelled = false;
    setCreatorsLoaded(false);
    paperGetCreators(paper.id)
      .then((cs) => {
        if (cancelled) return;
        setCreators(cs);
        setCreatorsLoaded(true);
      })
      .catch(() => {
        if (!cancelled) setCreatorsLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [paper.id]);
  useEffect(() => {
    if (!creatorsLoaded) return;
    const nameOf = (c: Creator) => c.name || [c.first_name, c.last_name].filter(Boolean).join(' ');
    setForm((f) => ({
      ...f,
      authors: creators
        .filter((c) => c.role === 'author')
        .map(nameOf)
        .filter(Boolean)
        .join('; '),
      editor: creators
        .filter((c) => c.role === 'editor')
        .map(nameOf)
        .filter(Boolean)
        .join('; '),
    }));
  }, [creators, creatorsLoaded]);

  // Paper's collections (Zotero-style display).
  useEffect(() => {
    paperGetCollections(paper.id)
      .then((cols) => setCollections(cols.map((c) => ({ id: c.id, name: c.name }))))
      .catch(() => setCollections([]));
  }, [paper.id]);

  const changed = useMemo(() => {
    return (
      form.title !== paper.title ||
      form.authors !== parseJsonArray(paper.authors).join('; ') ||
      form.year !== (paper.year?.toString() ?? '') ||
      form.journal !== (paper.journal ?? '') ||
      form.doi !== (paper.doi ?? '') ||
      form.url !== (paper.url ?? '') ||
      form.abstract !== (paper.abstract_text ?? '') ||
      form.keywords !== parseJsonArray(paper.keywords).join(', ') ||
      form.citationKey !== (paper.citation_key ?? '') ||
      form.itemType !== (paper.item_type ?? 'journal') ||
      form.volume !== (paper.volume ?? '') ||
      form.issue !== (paper.issue ?? '') ||
      form.pages !== (paper.pages ?? '') ||
      form.conferenceName !== (paper.conference_name ?? '') ||
      form.publisher !== (paper.publisher ?? '') ||
      form.place !== (paper.place ?? '') ||
      form.editor !== parseJsonArray(paper.editor).join('; ') ||
      form.series !== (paper.series ?? '') ||
      form.edition !== (paper.edition ?? '') ||
      form.isbn !== (paper.isbn ?? '') ||
      form.issn !== (paper.issn ?? '') ||
      form.language !== (paper.language ?? '') ||
      form.numPages !== (paper.num_pages?.toString() ?? paper.page_count?.toString() ?? '') ||
      form.archiveLocation !== (paper.archive_location ?? '') ||
      form.callNumber !== (paper.call_number ?? '') ||
      form.rights !== (paper.rights ?? '')
    );
  }, [form, paper]);

  const handleSave = () => {
    // The 页数 field falls back to the physical PDF page count for display;
    // that fallback must not be persisted into num_pages (it would block
    // CrossRef from filling the true bibliographic value later).
    const numPagesToSave =
      paper.num_pages == null && form.numPages === (paper.page_count?.toString() ?? '')
        ? null
        : form.numPages
          ? parseInt(form.numPages, 10) || null
          : null;
    // Persist structured creators (also regenerates authors/editor columns).
    paperSetCreators(paper.id, creators).catch((err) =>
      console.error('保存创作者失败:', err)
    );
    updateMutation.mutate({
      id: paper.id,
      input: {
        title: form.title,
        authors: form.authors.split(/;\s*|，\s*|,\s*/).filter(Boolean),
        year: form.year ? parseInt(form.year, 10) : null,
        journal: form.journal || null,
        doi: form.doi || null,
        url: form.url || null,
        abstract_text: form.abstract || null,
        keywords: form.keywords.split(/,\s*|，\s*/).filter(Boolean),
        item_type: form.itemType || null,
        volume: form.volume || null,
        issue: form.issue || null,
        pages: form.pages || null,
        conference_name: form.conferenceName || null,
        publisher: form.publisher || null,
        place: form.place || null,
        editor: form.editor.split(/;\s*|，\s*|,\s*/).filter(Boolean),
        series: form.series || null,
        edition: form.edition || null,
        isbn: form.isbn || null,
        issn: form.issn || null,
        language: form.language || null,
        num_pages: numPagesToSave,
        archive_location: form.archiveLocation || null,
        call_number: form.callNumber || null,
        rights: form.rights || null,
      },
    });
  };

  const hiddenFields = useMemo(
    () => new Set(HIDDEN_FIELDS_BY_TYPE[form.itemType] ?? []),
    [form.itemType]
  );

  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 overflow-y-auto p-4 space-y-3">
        <button
          onClick={handleEnrich}
          disabled={enriching}
          className="w-full flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary/10 text-primary text-xs hover:bg-primary/20 disabled:opacity-50 transition-colors"
          title="通过 DOI 或标题从 CrossRef 自动补全空白的年份、期刊、卷期、页码、摘要等"
        >
          {enriching ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
          {enriching ? '正在查询 CrossRef…' : '自动补充元数据'}
        </button>
        <Field label="标题" value={form.title} onChange={(v) => setForm((f) => ({ ...f, title: v }))} />
        <div>
          <span className="text-[11px] font-medium text-text-secondary/70 uppercase tracking-wider">创作者</span>
          <div className="mt-1">
            <CreatorEditor creators={creators} onChange={setCreators} />
          </div>
        </div>
        <SelectField
          label="文献类型"
          value={form.itemType}
          options={ITEM_TYPES}
          onChange={(v) => setForm((f) => ({ ...f, itemType: v }))}
        />
        <div className="grid grid-cols-2 gap-3">
          <Field label="年份" value={form.year} onChange={(v) => setForm((f) => ({ ...f, year: v }))} />
          {!hiddenFields.has('journal') && (
            <Field label="期刊" value={form.journal} onChange={(v) => setForm((f) => ({ ...f, journal: v }))} />
          )}
          {!hiddenFields.has('volume') && (
            <Field label="卷" value={form.volume} onChange={(v) => setForm((f) => ({ ...f, volume: v }))} />
          )}
          {!hiddenFields.has('issue') && (
            <Field label="期" value={form.issue} onChange={(v) => setForm((f) => ({ ...f, issue: v }))} />
          )}
          {!hiddenFields.has('pages') && (
            <Field label="页码" value={form.pages} onChange={(v) => setForm((f) => ({ ...f, pages: v }))} />
          )}
          {!hiddenFields.has('numPages') && (
            <Field label="页数" value={form.numPages} onChange={(v) => setForm((f) => ({ ...f, numPages: v }))} />
          )}
          {!hiddenFields.has('conferenceName') && (
            <Field label="会议名称" value={form.conferenceName} onChange={(v) => setForm((f) => ({ ...f, conferenceName: v }))} />
          )}
          {!hiddenFields.has('publisher') && (
            <Field label="出版社" value={form.publisher} onChange={(v) => setForm((f) => ({ ...f, publisher: v }))} />
          )}
          {!hiddenFields.has('place') && (
            <Field label="出版地" value={form.place} onChange={(v) => setForm((f) => ({ ...f, place: v }))} />
          )}
          {!hiddenFields.has('series') && (
            <Field label="系列" value={form.series} onChange={(v) => setForm((f) => ({ ...f, series: v }))} />
          )}
          {!hiddenFields.has('edition') && (
            <Field label="版本" value={form.edition} onChange={(v) => setForm((f) => ({ ...f, edition: v }))} />
          )}
          <Field label="语言" value={form.language} onChange={(v) => setForm((f) => ({ ...f, language: v }))} />
        </div>
        <Field label="DOI" value={form.doi} onChange={(v) => setForm((f) => ({ ...f, doi: v }))} />
        <Field label="URL" value={form.url} onChange={(v) => setForm((f) => ({ ...f, url: v }))} />
        <div className="grid grid-cols-2 gap-3">
          {!hiddenFields.has('isbn') && (
            <Field label="ISBN" value={form.isbn} onChange={(v) => setForm((f) => ({ ...f, isbn: v }))} />
          )}
          {!hiddenFields.has('issn') && (
            <Field label="ISSN" value={form.issn} onChange={(v) => setForm((f) => ({ ...f, issn: v }))} />
          )}
        </div>
        <Field label="引用键" value={form.citationKey} onChange={(v) => setForm((f) => ({ ...f, citationKey: v }))} />
        <Field label="关键词" value={form.keywords} placeholder="用逗号分隔" onChange={(v) => setForm((f) => ({ ...f, keywords: v }))} />
        <div className="grid grid-cols-2 gap-3">
          {!hiddenFields.has('archiveLocation') && (
            <Field label="归档位置" value={form.archiveLocation} onChange={(v) => setForm((f) => ({ ...f, archiveLocation: v }))} />
          )}
          {!hiddenFields.has('callNumber') && (
            <Field label="索书号" value={form.callNumber} onChange={(v) => setForm((f) => ({ ...f, callNumber: v }))} />
          )}
        </div>
        <Field label="权利" value={form.rights} onChange={(v) => setForm((f) => ({ ...f, rights: v }))} />
        {!hiddenFields.has('abstract') && (
          <Field label="摘要" value={form.abstract} multiline placeholder="文献摘要..." onChange={(v) => setForm((f) => ({ ...f, abstract: v }))} />
        )}

        {/* Collections & timestamps (Zotero-style) */}
        {collections.length > 0 && (
          <div className="flex items-start gap-1.5 text-xs text-text-secondary/70">
            <span className="shrink-0">收藏于：</span>
            <span>{collections.map((c) => c.name).join('、')}</span>
          </div>
        )}
        <div className="text-[10px] text-text-secondary/40">
          创建于 {formatDate(paper.created_at)} · 更新于 {formatDate(paper.updated_at)}
        </div>

        {/* Attachments (the main PDF is the first row) */}
        <div className="pt-2 border-t border-surface-hover space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium text-text-primary">附件</span>
            <button
              onClick={handleAddAttachment}
              className="flex items-center gap-1 text-xs text-text-secondary hover:text-primary transition-colors"
            >
              <Plus size={12} /> 添加附件
            </button>
          </div>
          {attachmentsLoading ? (
            <div className="text-xs text-text-secondary/60">加载中...</div>
          ) : attachments.length === 0 ? (
            <div className="text-xs text-text-secondary/40">暂无附件</div>
          ) : (
            <div className="space-y-1">
              {attachments.map((a) => (
                <div key={a.id} className="flex items-center gap-2 group">
                  <Paperclip size={12} className="text-text-secondary/50 shrink-0" />
                  <button
                    onClick={() => paperOpenAttachment(a.id).catch((err) => alert(`打开失败: ${err}`))}
                    className="flex-1 min-w-0 text-left truncate text-xs text-text-primary hover:text-primary transition-colors"
                    title={a.file_path}
                  >
                    {a.file_name}
                  </button>
                  <button
                    onClick={() => handleRemoveAttachment(a.id)}
                    className="opacity-0 group-hover:opacity-100 text-text-secondary/50 hover:text-red-400 transition-opacity"
                    title="删除附件"
                  >
                    <X size={12} />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Export annotations to Markdown */}
        <button
          onClick={handleExportAnnotations}
          className="w-full flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg bg-surface border border-surface-hover text-text-secondary text-xs hover:bg-surface-hover transition-colors"
        >
          <FileDown size={13} /> 导出标注（Markdown）
        </button>

        {/* Related papers */}
        <div className="pt-2 border-t border-surface-hover space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium text-text-primary">相关文献</span>
            <button
              onClick={handleAddRelated}
              className="flex items-center gap-1 text-xs text-text-secondary hover:text-primary transition-colors"
            >
              <Link2 size={12} /> 添加关联
            </button>
          </div>
          {relatedLoading ? (
            <div className="text-xs text-text-secondary/60">加载中...</div>
          ) : related.length === 0 ? (
            <div className="text-xs text-text-secondary/40">暂无关联文献，可手动关联相关条目</div>
          ) : (
            <div className="space-y-1">
              {related.map((r) => (
                <div key={r.id} className="flex items-center gap-2 group">
                  <button
                    onClick={() => {
                      useTabStore.getState().open({
                        id: `reader-${r.id}`,
                        title: r.title || '未命名',
                        icon: 'pdf',
                        route: '/reader/$paperId',
                        params: { paperId: r.id },
                      });
                    }}
                    className="flex-1 min-w-0 text-left truncate text-xs text-text-primary hover:text-primary transition-colors"
                    title={r.title}
                  >
                    {r.title || '未命名文献'}
                  </button>
                  <button
                    onClick={() => handleRemoveRelated(r.id)}
                    className="opacity-0 group-hover:opacity-100 text-text-secondary/50 hover:text-red-400 transition-opacity"
                    title="取消关联"
                  >
                    <X size={12} />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {changed && (
        <div className="p-3 border-t border-surface-hover flex items-center justify-end gap-2 bg-surface/50">
          <button
            onClick={() =>
              setForm({
                title: paper.title,
                authors: parseJsonArray(paper.authors).join('; '),
                year: paper.year?.toString() ?? '',
                journal: paper.journal ?? '',
                doi: paper.doi ?? '',
                url: paper.url ?? '',
                abstract: paper.abstract_text ?? '',
                keywords: parseJsonArray(paper.keywords).join(', '),
                citationKey: paper.citation_key ?? '',
                itemType: paper.item_type ?? 'journal',
                volume: paper.volume ?? '',
                issue: paper.issue ?? '',
                pages: paper.pages ?? '',
                conferenceName: paper.conference_name ?? '',
                publisher: paper.publisher ?? '',
                place: paper.place ?? '',
                editor: parseJsonArray(paper.editor).join('; '),
                series: paper.series ?? '',
                edition: paper.edition ?? '',
                isbn: paper.isbn ?? '',
                issn: paper.issn ?? '',
                language: paper.language ?? '',
                numPages: paper.num_pages?.toString() ?? paper.page_count?.toString() ?? '',
                archiveLocation: paper.archive_location ?? '',
                callNumber: paper.call_number ?? '',
                rights: paper.rights ?? '',
              })
            }
            className="px-3 py-1.5 rounded-lg text-xs text-text-secondary hover:bg-surface-hover transition-colors"
          >
            重置
          </button>
          <button
            onClick={handleSave}
            disabled={updateMutation.isPending}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary/10 text-primary text-xs font-medium hover:bg-primary/20 transition-colors disabled:opacity-50"
          >
            <Save size={13} />
            {updateMutation.isPending ? '保存中...' : '保存'}
          </button>
        </div>
      )}
    </div>
  );
}

export function NotesTab({ paperId }: { paperId: string }) {
  const { data: notes, isLoading, refetch } = usePaperNotes(paperId);
  const { data: paper } = usePaper(paperId);
  const { prompt, confirm } = useDialog();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [allNotes, setAllNotes] = useState<Note[]>([]);
  const [backlinks, setBacklinks] = useState<{ id: string; title: string; context: string; created_at: string }[]>([]);
  const [editorKey, setEditorKey] = useState(0);

  // Full vault notes (wiki links / backlinks resolve against these).
  useEffect(() => {
    notesListAll().then(setAllNotes).catch(() => {});
  }, []);

  const selected = notes?.find((n) => n.id === selectedId) ?? null;

  useEffect(() => {
    if (selectedId) {
      notesGetBacklinks(selectedId).then(setBacklinks).catch(() => setBacklinks([]));
    } else {
      setBacklinks([]);
    }
  }, [selectedId]);

  const refresh = async () => {
    await refetch();
  };

  const handleUpdate = async (id: string, title: string, content: string) => {
    await notesUpdate(id, title, content, undefined);
    await refresh();
  };

  const handleDelete = async (id: string) => {
    await notesDelete(id);
    setSelectedId(null);
    await refresh();
  };

  const handleToggleFavorite = async (id: string, fav: boolean) => {
    await notesUpdate(id, undefined, undefined, undefined, undefined, fav ? 1 : 0);
    await refresh();
  };

  const handleVersionRestored = async () => {
    await refresh();
    setEditorKey((k) => k + 1);
  };

  const handleCreate = async () => {
    const title = await prompt('笔记标题', {
      title: '新建笔记',
      defaultValue: paper?.title ?? '',
      placeholder: paper?.title ? undefined : '输入笔记标题',
    });
    if (title === null) return; // cancelled
    const n = await noteCreateUnderPaper(paperId, title.trim() || '新笔记', '');
    await refresh();
    setSelectedId(n.id);
  };

  const handleMergeIntoExcerpt = async (note: Note) => {
    const ok = await confirm(
      `将「${note.title}」的内容并入文献笔记（摘录），并删除该笔记？`,
      '并入文献笔记'
    );
    if (!ok) return;
    try {
      await noteMergeIntoExcerpt(note.id, paperId);
      await refresh();
    } catch (err) {
      console.error('merge into excerpt:', err);
    }
  };

  const handleCreateLink = async (title: string) => {
    const n = await noteCreateUnderPaper(paperId, title, '');
    await refresh();
    setSelectedId(n.id);
  };

  // Editing view: embed the full NoteEditor for the selected note.
  if (selected) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-2 border-b border-surface-hover flex items-center gap-2 shrink-0">
          <button
            onClick={() => setSelectedId(null)}
            className="flex items-center gap-1 text-xs text-text-secondary hover:text-text-primary transition-colors"
          >
            <ArrowLeft size={13} /> 返回列表
          </button>
          <span className="text-[11px] text-text-secondary/60 truncate">{selected.title}</span>
        </div>
        <NoteEditor
          key={editorKey}
          note={selected}
          notes={allNotes}
          onUpdate={handleUpdate}
          onNavigate={(id) => setSelectedId(id)}
          onCreateLink={handleCreateLink}
          onDelete={handleDelete}
          onToggleFavorite={handleToggleFavorite}
          backlinkCount={backlinks.length}
          backlinks={backlinks}
          onVersionRestored={handleVersionRestored}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      <div className="p-3 border-b border-surface-hover flex items-center justify-between">
        <span className="text-xs text-text-secondary">{notes?.length ?? 0} 条笔记</span>
        <button
          onClick={handleCreate}
          className="flex items-center gap-1 px-2 py-1 rounded-lg bg-primary/10 text-primary text-xs hover:bg-primary/20 transition-colors"
        >
          <Plus size={12} /> 新建
        </button>
      </div>
      <div className="flex-1 overflow-y-auto p-2">
        {isLoading ? (
          <div className="p-4 text-center text-xs text-text-secondary/60">加载中...</div>
        ) : notes?.length === 0 ? (
          <div className="p-6 text-center text-xs text-text-secondary/60">
            <StickyNote size={32} className="mx-auto mb-2 opacity-40" />
            暂无笔记
          </div>
        ) : (
          <div className="space-y-1">
            {notes?.map((note) => (
              <div
                key={note.id}
                className="group flex items-center gap-2 rounded-lg bg-surface border border-surface-hover hover:border-primary/30 transition-colors"
              >
                <button
                  onClick={() => setSelectedId(note.id)}
                  className="flex-1 min-w-0 text-left px-3 py-2"
                >
                  <div className="flex items-center gap-1.5 text-xs font-medium text-text-primary truncate">
                    <span className="truncate">{note.title}</span>
                    {note.is_excerpt === 1 && (
                      <span className="shrink-0 text-[9px] px-1 py-px rounded bg-primary/15 text-primary leading-none">
                        摘录
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] text-text-secondary/60 truncate mt-0.5">
                    {note.content_plain.slice(0, 80)}
                  </div>
                </button>
                {note.is_excerpt !== 1 && note.content.trim() && (
                  <button
                    onClick={() => handleMergeIntoExcerpt(note)}
                    className="shrink-0 mr-2 px-1.5 py-0.5 rounded text-[10px] text-text-secondary/60 opacity-0 group-hover:opacity-100 hover:text-primary hover:bg-primary/10 transition-all"
                    title="并入文献笔记（摘录）"
                  >
                    并入
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function TagsTab({ paperId }: { paperId: string }) {
  const { data: paperTags, isLoading: tagsLoading } = usePaperTags(paperId);
  const { data: allTags } = useTags();
  const addMutation = useAddTagsToPaper();
  const removeMutation = useRemoveTagsFromPaper();
  const createTag = useCreateTag();
  const { prompt } = useDialog();
  const [open, setOpen] = useState(false);

  const paperTagIds = useMemo(() => new Set(paperTags?.map((t) => t.id) ?? []), [paperTags]);
  const availableTags = useMemo(() => allTags?.filter((t) => !paperTagIds.has(t.id)) ?? [], [allTags, paperTagIds]);

  return (
    <div className="flex flex-col h-full p-4">
      <div className="flex items-center justify-between mb-3">
        <span className="text-xs text-text-secondary">{paperTags?.length ?? 0} 个标签</span>
        <div className="relative">
          <button
            onClick={() => setOpen((v) => !v)}
            className="flex items-center gap-1 px-2 py-1 rounded-lg bg-surface border border-surface-hover text-xs text-text-secondary hover:text-text-primary transition-colors"
          >
            <Plus size={12} /> 添加 <ChevronDown size={12} />
          </button>
          {open && (
            <div className="absolute right-0 top-full mt-1 w-40 bg-surface border border-surface-hover rounded-lg shadow-xl z-20 py-1 max-h-48 overflow-y-auto">
              {availableTags.length === 0 ? (
                <div className="px-3 py-2 text-xs text-text-secondary/60">没有可用标签</div>
              ) : (
                availableTags.map((tag) => (
                  <button
                    key={tag.id}
                    onClick={() => {
                      addMutation.mutate({ paperId, tagIds: [tag.id] });
                      setOpen(false);
                    }}
                    className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-text-secondary hover:bg-surface-hover hover:text-text-primary transition-colors"
                  >
                    <Tag size={11} style={{ color: tag.color }} />
                    <span className="truncate">{tag.name}</span>
                  </button>
                ))
              )}
              <div className="border-t border-surface-hover my-1" />
              <button
                onClick={async () => {
                  const name = await prompt('新建标签名称', { title: '新建标签' });
                  if (name?.trim()) {
                    createTag.mutate(
                      { name: name.trim() },
                      {
                        onSuccess: (tag) => {
                          addMutation.mutate({ paperId, tagIds: [tag.id] });
                        },
                      }
                    );
                  }
                  setOpen(false);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-primary hover:bg-primary/10 transition-colors"
              >
                <Plus size={11} /> 新建标签
              </button>
            </div>
          )}
        </div>
      </div>

      {tagsLoading ? (
        <div className="text-xs text-text-secondary/60">加载中...</div>
      ) : paperTags?.length === 0 ? (
        <div className="flex-1 flex flex-col items-center justify-center text-text-secondary/60 text-center">
          <Tag size={40} className="mb-2 opacity-40" />
          <p className="text-sm">暂无标签</p>
          <p className="text-xs mt-1 opacity-60">点击右上角添加标签</p>
        </div>
      ) : (
        <div className="flex flex-wrap gap-2">
          {paperTags?.map((tag) => (
            <span
              key={tag.id}
              className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs bg-surface-hover text-text-secondary border border-surface-hover"
            >
              <Tag size={11} style={{ color: tag.color }} />
              {tag.name}
              <button
                onClick={() => removeMutation.mutate({ paperId, tagIds: [tag.id] })}
                className="ml-0.5 p-0.5 rounded hover:bg-surface-hover text-text-secondary/50 hover:text-red-400 transition-colors"
              >
                <Trash2 size={10} />
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

const tabs: { key: TabKey; label: string; icon: React.ReactNode }[] = [
  { key: 'info', label: '信息', icon: <Info size={18} /> },
  { key: 'notes', label: '笔记', icon: <StickyNote size={18} /> },
  { key: 'tags', label: '标签', icon: <Tag size={18} /> },
];

function DetailIconBar({
  activeTab,
  onSelect,
}: {
  activeTab: TabKey;
  onSelect: (tab: TabKey) => void;
}) {
  const setRightPanelCollapsed = useLibraryStore((s) => s.setRightPanelCollapsed);

  return (
    <div className="flex flex-col h-full items-center py-2 bg-surface/30">
      <button
        onClick={() => setRightPanelCollapsed(false)}
        className="w-9 h-9 rounded flex items-center justify-center mb-1 text-text-secondary hover:text-text-primary hover:bg-surface-hover/60 transition-colors"
        title="展开详情边栏"
      >
        <SidebarToggleIcon collapsed={true} side="right" />
      </button>
      <div className="w-5 h-px bg-surface-hover my-1" />
      {tabs.map((tab) => (
        <button
          key={tab.key}
          onClick={() => {
            onSelect(tab.key);
            setRightPanelCollapsed(false);
          }}
          title={tab.label}
          className={`w-9 h-9 rounded flex items-center justify-center mb-1 transition-colors ${
            activeTab === tab.key
              ? 'bg-surface-hover text-primary'
              : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/60'
          }`}
        >
          {tab.icon}
        </button>
      ))}
    </div>
  );
}

export function PaperDetailPanel() {
  const selectedIds = useLibraryStore((s) => s.selectedPaperIds);
  const rightPanelCollapsed = useLibraryStore((s) => s.rightPanelCollapsed);
  const setRightPanelCollapsed = useLibraryStore((s) => s.setRightPanelCollapsed);
  const [activeTab, setActiveTab] = useState<TabKey>('info');

  const selectedId = selectedIds.length === 1 ? selectedIds[0] : null;
  const { data: paper, isLoading } = usePaper(selectedId);

  useEffect(() => {
    if (selectedIds.length !== 1) setActiveTab('info');
  }, [selectedIds]);

  // Expose the focused paper to the global pet.
  useEffect(() => {
    if (paper) {
      usePetContextStore.getState().setContext({
        page: 'library',
        objectId: paper.id,
        title: paper.title || '未命名文献',
      });
    } else {
      usePetContextStore.getState().setContext(null);
    }
    return () => usePetContextStore.getState().setContext(null);
  }, [paper]);

  if (rightPanelCollapsed) {
    return <DetailIconBar activeTab={activeTab} onSelect={setActiveTab} />;
  }

  if (selectedIds.length === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-text-secondary/60 p-6 text-center">
        <Info size={40} className="mb-3 opacity-40" />
        <p className="text-sm">选择一篇文献查看详情</p>
        <p className="text-xs mt-1 opacity-60">在此编辑元数据、查看笔记、管理附件与标签</p>
      </div>
    );
  }

  if (selectedIds.length > 1) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-text-secondary/60 p-6 text-center">
        <p className="text-sm">已选择 {selectedIds.length} 篇文献</p>
        <p className="text-xs mt-1 opacity-60">请只选择一篇文献以查看详情</p>
      </div>
    );
  }

  if (isLoading || !paper) {
    return (
      <div className="flex-1 flex items-center justify-center text-text-secondary/60">
        <span className="text-xs">加载中...</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-surface-hover shrink-0">
        <FileText size={16} className="text-primary shrink-0" />
        <h3 className="flex-1 min-w-0 text-sm font-medium text-text-primary truncate" title={paper.title}>
          {paper.title || '未命名文献'}
        </h3>
        <button
          onClick={() => setRightPanelCollapsed(true)}
          className="h-[28px] w-[28px] flex items-center justify-center rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors shrink-0"
          title="关闭详情边栏"
        >
          <SidebarToggleIcon collapsed={false} side="right" />
        </button>
      </div>

      {/* Tabs */}
      <div className="flex items-center border-b border-surface-hover shrink-0">
        {tabs.map((tab) => (
          <button
            key={tab.key}
            onClick={() => setActiveTab(tab.key)}
            className={`flex-1 px-2 py-2 text-xs font-medium transition-colors ${
              activeTab === tab.key
                ? 'text-primary border-b-2 border-primary bg-primary/5'
                : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/50'
            }`}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      <div className="flex-1 min-h-0 overflow-hidden">
        {activeTab === 'info' && <InfoTab paper={paper} />}
        {activeTab === 'notes' && <NotesTab paperId={paper.id} />}
        {activeTab === 'tags' && <TagsTab paperId={paper.id} />}
      </div>
    </div>
  );
}
