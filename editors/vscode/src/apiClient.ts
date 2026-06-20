import * as vscode from 'vscode';

export interface ChatMessage {
    role: 'user' | 'assistant' | 'system';
    content: string;
    toolCalls?: ToolCallInfo[];
}

export interface ToolCallInfo {
    name: string;
    status: 'running' | 'done' | 'error';
}

export interface StreamEvent {
    type: string;
    content?: string;
    usage?: { input_tokens: number; output_tokens: number; total_tokens: number };
}

export class ApiClient {
    private baseUrl: string;
    private timeout: number;
    private abortController: AbortController | null = null;

    constructor(baseUrl: string, timeout: number) {
        this.baseUrl = baseUrl;
        this.timeout = timeout;
    }

    async checkHealth(): Promise<boolean> {
        try {
            const resp = await fetch(`${this.baseUrl}/health`, { 
                signal: AbortSignal.timeout(5000) 
            });
            return resp.ok;
        } catch {
            return false;
        }
    }

    async *chatStream(message: string, systemPrompt?: string): AsyncGenerator<StreamEvent> {
        this.abortController = new AbortController();
        
        const body: any = { message };
        if (systemPrompt) {
            body.system_prompt = systemPrompt;
        }

        try {
            const resp = await fetch(`${this.baseUrl}/api/v1/chat/stream`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(body),
                signal: this.abortController.signal,
            });

            if (!resp.ok) {
                const text = await resp.text().catch(() => 'Unknown error');
                yield { type: 'error', content: `HTTP ${resp.status}: ${text}` };
                return;
            }

            const reader = resp.body!.getReader();
            const decoder = new TextDecoder();
            let buffer = '';

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;

                buffer += decoder.decode(value, { stream: true });
                const lines = buffer.split('\n');
                buffer = lines.pop() || '';

                for (const line of lines) {
                    if (!line.startsWith('data: ')) continue;
                    const data = line.slice(6);
                    if (data === '[DONE]' || !data) continue;

                    try {
                        const event: StreamEvent = JSON.parse(data);
                        yield event;
                    } catch {
                        // 忽略解析失败的行
                    }
                }
            }

            // 处理剩余缓冲
            if (buffer.startsWith('data: ')) {
                const data = buffer.slice(6);
                if (data && data !== '[DONE]') {
                    try {
                        yield JSON.parse(data);
                    } catch { /* ignore */ }
                }
            }
        } catch (e: any) {
            if (e.name === 'AbortError') {
                yield { type: 'error', content: 'Request cancelled' };
            } else {
                yield { type: 'error', content: `Connection error: ${e.message}` };
            }
        }
    }

    async sendMessage(message: string): Promise<string> {
        const resp = await fetch(`${this.baseUrl}/api/v1/chat`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ message }),
        });

        if (!resp.ok) {
            throw new Error(`HTTP ${resp.status}`);
        }

        const data = await resp.json();
        return data.content || data.message || 'No response';
    }

    cancel() {
        this.abortController?.abort();
    }

    dispose() {
        this.cancel();
    }
}
