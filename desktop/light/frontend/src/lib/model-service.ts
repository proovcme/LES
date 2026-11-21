export function serviceLabel(extension: string | null): string {
  const names: Record<string, string> = { ollama: 'Ollama', mlx: 'MLX', lm_studio: 'LM Studio',
    llama_cpp: 'llama.cpp', lemonade: 'Lemonade', freetoken: 'FreeToken' };
  return names[extension || ''] || 'Совместимый API';
}
