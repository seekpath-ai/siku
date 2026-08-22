import { useState, useEffect } from 'react';
import { useKnowledgeStore } from '@/stores/knowledgeStore';
import {
  knowledgeListDomains, knowledgeListItems, knowledgeCreateItem,
  knowledgeUpdateItem, knowledgeDeleteItem,
} from '@/lib/tauri';

const PAGE_SIZE = 20;

export function useKnowledge() {
  const store = useKnowledgeStore();
  const [page, setPage] = useState(0);

  useEffect(() => { loadDomains(); }, []);

  useEffect(() => {
    setPage(0);
    loadItems(0);
  }, [store.activeDomainId, store.searchQuery]);

  const loadDomains = async () => {
    try { store.setDomains(await knowledgeListDomains()); }
    catch (err) { console.error('load domains:', err); }
  };

  const loadItems = async (p: number) => {
    store.setLoading(true);
    try {
      const items = await knowledgeListItems(
        store.activeDomainId || undefined,
        store.searchQuery || undefined,
        undefined, PAGE_SIZE, p * PAGE_SIZE,
      );
      store.setItems(items);
      setPage(p);
    } catch (err) { console.error('load items:', err); }
    finally { store.setLoading(false); }
  };

  const createItem = async (domainId: string, title: string, content?: string) => {
    try { await knowledgeCreateItem(domainId, title, content); await loadItems(page); return true; }
    catch (err) { console.error(err); return false; }
  };

  const updateItem = async (id: string, title: string, content: string) => {
    try { await knowledgeUpdateItem(id, title, content); await loadItems(page); }
    catch (err) { console.error(err); }
  };

  const deleteItem = async (id: string) => {
    try { await knowledgeDeleteItem(id); store.setItems(store.items.filter((i) => i.id !== id)); }
    catch (err) { console.error(err); }
  };

  const nextPage = () => loadItems(page + 1);
  const prevPage = () => { if (page > 0) loadItems(page - 1); };

  return {
    ...store, page, hasMore: store.items.length >= PAGE_SIZE,
    loadItems, createItem, updateItem, deleteItem,
    nextPage, prevPage,
  };
}
