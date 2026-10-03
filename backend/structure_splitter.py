"""Document chunks respecting headings, paragraphs, lists and tables."""
import re
from llama_index.core.node_parser import SentenceSplitter
from llama_index.core.schema import TextNode

class StructureAwareSplitter:
    """Structure-aware chunking for SP and GOST documents.
    Preserves numbered clauses (e.g. 5.2.1) as single indivisible blocks.
    Fits chunks within a target character length, and implements sentence-bounded overlap.
    """
    def __init__(self, chunk_size: int, chunk_overlap: int, len_fn=None):
        # W2.1 (ADR-7): len_fn — счётчик размера (токены эмбеддера); None = символы.
        self.chunk_size = chunk_size
        self.chunk_overlap = chunk_overlap
        self._len = len_fn or len
        # Жёсткая нарезка патологически длинных предложений — всегда в символах:
        # при токенном режиме берём ~3 символа на токен (русский текст).
        self._hard_slice_chars = chunk_size if len_fn is None else chunk_size * 3
        self.fallback = SentenceSplitter(chunk_size=chunk_size, chunk_overlap=chunk_overlap)
        
        # Regex to detect lines that start a new numbered section or markdown header
        self.boundary_pattern = re.compile(
            r"^(?:#{1,6}\s+|"
            r"(?:Пункт|Раздел|Статья|п\.|§)\s*\d+(?:\.\d+)+|"
            r"\d+(?:\.\d+)+(?:\s+|\.|$))",
            re.IGNORECASE
        )

    def _split_into_atomic_blocks(self, text: str) -> list[str]:
        lines = text.split("\n")
        blocks = []
        current_block_lines = []
        
        for line in lines:
            stripped = line.strip()
            if not stripped:
                if current_block_lines:
                    current_block_lines.append(line)
                continue
                
            if self.boundary_pattern.match(stripped):
                if current_block_lines:
                    blocks.append("\n".join(current_block_lines).strip())
                    current_block_lines = []
            
            current_block_lines.append(line)
            
        if current_block_lines:
            blocks.append("\n".join(current_block_lines).strip())
            
        return [b for b in blocks if b]

    def _get_sentence_overlap(self, text_prev: str, max_overlap: int) -> str:
        if not text_prev or max_overlap <= 0:
            return ""
        sentences = re.split(r'(?<=[.!?])\s+', text_prev)
        overlap_sentences = []
        current_len = 0
        for s in reversed(sentences):
            s = s.strip()
            if not s:
                continue
            if current_len + self._len(s) + 1 <= max_overlap:
                overlap_sentences.append(s)
                current_len += self._len(s) + 1
            else:
                if not overlap_sentences:
                    return s[-max_overlap * (1 if self._len is len else 3):]
                break
        if not overlap_sentences:
            return ""
        return " ".join(reversed(overlap_sentences)) + " "

    def _split_large_block(self, text: str, max_chars: int, overlap_chars: int) -> list[str]:
        sentences = re.split(r'(?<=[.!?])\s+', text)
        chunks = []
        current_chunk = []
        current_len = 0
        
        for s in sentences:
            s = s.strip()
            if not s:
                continue
            s_len = self._len(s)

            if s_len > max_chars:
                if current_chunk:
                    chunks.append(" ".join(current_chunk))
                    current_chunk = []
                    current_len = 0
                hard_max = self._hard_slice_chars
                hard_overlap = min(overlap_chars * (1 if self._len is len else 3), hard_max // 4)
                raw_len = len(s)
                i = 0
                while i < raw_len:
                    chunks.append(s[i:i + hard_max])
                    i += hard_max - hard_overlap
                    if i + hard_overlap >= raw_len:
                        if i < raw_len:
                            chunks.append(s[i:])
                        break
            else:
                separator_len = 1 if current_chunk else 0
                if current_len + separator_len + s_len <= max_chars:
                    current_chunk.append(s)
                    current_len += separator_len + s_len
                else:
                    chunks.append(" ".join(current_chunk))
                    overlap_prefix = self._get_sentence_overlap(chunks[-1], overlap_chars)
                    current_chunk = []
                    current_len = 0
                    if overlap_prefix:
                        current_chunk.append(overlap_prefix.strip())
                        current_len = self._len(overlap_prefix.strip())

                    separator_len = 1 if current_chunk else 0
                    current_chunk.append(s)
                    current_len += separator_len + s_len
                    
        if current_chunk:
            chunks.append(" ".join(current_chunk))
        return chunks

    def get_nodes_from_documents(self, documents: list) -> list:
        all_nodes = []
        for doc in documents:
            text = doc.text
            metadata = doc.metadata or {}
            doc_id = doc.node_id if hasattr(doc, "node_id") else doc.id_
            
            atomic_blocks = self._split_into_atomic_blocks(text)
            
            chunks = []
            current_chunk_blocks = []
            current_chunk_len = 0
            
            for block in atomic_blocks:
                block_len = self._len(block)
                
                if block_len > self.chunk_size:
                    if current_chunk_blocks:
                        chunks.append("\n\n".join(current_chunk_blocks))
                        current_chunk_blocks = []
                        current_chunk_len = 0
                    
                    sub_chunks = self._split_large_block(block, self.chunk_size, self.chunk_overlap)
                    chunks.extend(sub_chunks)
                else:
                    separator_len = 2 if current_chunk_blocks else 0
                    if current_chunk_len + separator_len + block_len <= self.chunk_size:
                        current_chunk_blocks.append(block)
                        current_chunk_len += separator_len + block_len
                    else:
                        chunks.append("\n\n".join(current_chunk_blocks))
                        overlap_prefix = self._get_sentence_overlap(chunks[-1], self.chunk_overlap)
                        
                        current_chunk_blocks = []
                        current_chunk_len = 0
                        if overlap_prefix:
                            current_chunk_blocks.append(overlap_prefix.strip())
                            current_chunk_len = self._len(overlap_prefix.strip())
                            
                        separator_len = 2 if current_chunk_blocks else 0
                        current_chunk_blocks.append(block)
                        current_chunk_len += separator_len + block_len
            
            if current_chunk_blocks:
                chunks.append("\n\n".join(current_chunk_blocks))
                
            for idx, chunk_text in enumerate(chunks):
                node = TextNode(
                    text=chunk_text,
                    id_=f"{doc_id}_chunk_{idx}",
                    metadata=metadata
                )
                all_nodes.append(node)
                
        return all_nodes
