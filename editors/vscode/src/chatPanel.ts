import * as vscode from 'vscode';
import { ApiClient, StreamEvent } from './apiClient';

export class ChatPanel {
    public static currentPanel: ChatPanel | undefined;
    private readonly _panel: vscode.WebviewPanel;
    private _disposables: vscode.Disposable[] = [];
    private _api: ApiClient;
    private _messages: Array<{ role: string; content: string }> = [];

    static createOrShow(extensionUri: vscode.Uri, api: ApiClient): ChatPanel {
        const column = vscode.ViewColumn.Two;

        if (ChatPanel.currentPanel) {
            ChatPanel.currentPanel._panel.reveal(column);
            return ChatPanel.currentPanel;
        }

        const panel = vscode.window.createWebviewPanel(
            'agentFrameworkChat',
            'Agent Chat',
            column,
            {
                enableScripts: true,
                retainContextWhenHidden: true,
                localResourceRoots: [vscode.Uri.joinPath(extensionUri, 'media')],
            }
        );

        ChatPanel.currentPanel = new ChatPanel(panel, extensionUri, api);
        return ChatPanel.currentPanel;
    }

    private constructor(
        panel: vscode.WebviewPanel,
        extensionUri: vscode.Uri,
        api: ApiClient
    ) {
        this._panel = panel;
        this._api = api;
        this._panel.webview.html = this._getHtml(this._panel.webview, extensionUri);

        // 消息处理
        this._panel.webview.onDidReceiveMessage(
            async (message) => {
                switch (message.command) {
                    case 'send':
                        await this._handleSend(message.text);
                        return;
                    case 'cancel':
                        this._api.cancel();
                        return;
                    case 'openSettings':
                        vscode.commands.executeCommand(
                            'workbench.action.openSettings',
                            'agentFramework'
                        );
                        return;
                    case 'checkHealth':
                        const ok = await this._api.checkHealth();
                        this._panel.webview.postMessage({
                            command: 'healthStatus',
                            connected: ok,
                        });
                        return;
                }
            },
            null,
            this._disposables
        );

        this._panel.onDidDispose(
            () => this.dispose(),
            null,
            this._disposables
        );

        // 初始健康检查
        this._api.checkHealth().then((ok) => {
            this._panel.webview.postMessage({
                command: 'healthStatus',
                connected: ok,
            });
        });
    }

    // 从命令发送消息
    sendMessage(text: string) {
        this._panel.webview.postMessage({
            command: 'addMessage',
            role: 'user',
            content: text,
        });
        this._messages.push({ role: 'user', content: text });
        this._handleSend(text);
    }

    private async _handleSend(text: string) {
        this._panel.webview.postMessage({ command: 'setLoading', loading: true });
        
        let assistantContent = '';
        let usage: { input: number; output: number } | null = null;

        try {
            for await (const event of this._api.chatStream(text)) {
                switch (event.type) {
                    case 'text':
                        assistantContent += event.content || '';
                        this._panel.webview.postMessage({
                            command: 'streamChunk',
                            content: assistantContent,
                        });
                        break;
                    case 'tool_call':
                        this._panel.webview.postMessage({
                            command: 'toolCall',
                            name: event.content || 'tool',
                        });
                        break;
                    case 'tool_result':
                        this._panel.webview.postMessage({
                            command: 'toolResult',
                            content: event.content || '',
                        });
                        break;
                    case 'usage':
                        if (event.usage) {
                            usage = {
                                input: event.usage.input_tokens,
                                output: event.usage.output_tokens,
                            };
                        }
                        break;
                    case 'done':
                        break;
                    case 'error':
                        this._panel.webview.postMessage({
                            command: 'streamChunk',
                            content: assistantContent + `\n\n**Error:** ${event.content}`,
                        });
                        break;
                }
            }
        } catch (e: any) {
            this._panel.webview.postMessage({
                command: 'streamChunk',
                content: assistantContent + `\n\n**Error:** ${e.message || 'Unknown error'}`,
            });
        }

        this._messages.push({ role: 'assistant', content: assistantContent });
        this._panel.webview.postMessage({ 
            command: 'setLoading', 
            loading: false,
            usage,
        });
    }

    private _getHtml(webview: vscode.Webview, extensionUri: vscode.Uri): string {
        return `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Agent Chat</title>
    <style>
        :root {
            --bg: var(--vscode-editor-background);
            --fg: var(--vscode-editor-foreground);
            --border: var(--vscode-panel-border);
            --accent: var(--vscode-focusBorder);
            --user-bg: var(--vscode-button-background);
            --user-fg: var(--vscode-button-foreground);
            --assistant-bg: var(--vscode-textBlockQuote-background);
            --input-bg: var(--vscode-input-background);
            --input-fg: var(--vscode-input-foreground);
            --tool-bg: var(--vscode-badge-background);
            --tool-fg: var(--vscode-badge-foreground);
            --error: var(--vscode-errorForeground);
        }
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body { 
            font-family: var(--vscode-font-family); 
            background: var(--bg); 
            color: var(--fg);
            height: 100vh; 
            display: flex; 
            flex-direction: column;
        }
        .header { 
            padding: 8px 12px; 
            border-bottom: 1px solid var(--border);
            display: flex; 
            align-items: center; 
            gap: 8px;
            font-size: 12px;
        }
        .status-dot { 
            width: 8px; 
            height: 8px; 
            border-radius: 50%; 
            background: var(--error);
            transition: background 0.3s;
        }
        .status-dot.connected { background: #3fb950; }
        .messages { 
            flex: 1; 
            overflow-y: auto; 
            padding: 12px;
            display: flex; 
            flex-direction: column; 
            gap: 12px;
        }
        .message { 
            padding: 10px 14px; 
            border-radius: 8px; 
            font-size: 13px; 
            line-height: 1.6;
            max-width: 90%;
            word-break: break-word;
        }
        .message.user { 
            align-self: flex-end; 
            background: var(--user-bg); 
            color: var(--user-fg);
        }
        .message.assistant { 
            align-self: flex-start; 
            background: var(--assistant-bg);
            border: 1px solid var(--border);
        }
        .message pre { 
            background: rgba(0,0,0,0.2); 
            padding: 8px; 
            border-radius: 4px;
            overflow-x: auto; 
            margin: 6px 0;
        }
        .message code { 
            font-family: var(--vscode-editor-font-family); 
            font-size: 12px;
        }
        .tool-call { 
            display: inline-flex; 
            align-items: center; 
            gap: 4px;
            padding: 2px 8px; 
            background: var(--tool-bg); 
            color: var(--tool-fg);
            border-radius: 4px; 
            font-size: 11px; 
            margin: 4px 0;
        }
        .input-area { 
            padding: 8px 12px; 
            border-top: 1px solid var(--border);
            display: flex; 
            gap: 8px;
        }
        .input-box { 
            flex: 1; 
            padding: 8px 12px; 
            background: var(--input-bg);
            color: var(--input-fg); 
            border: 1px solid var(--border);
            border-radius: 6px; 
            font-family: inherit; 
            font-size: 13px;
            resize: vertical; 
            min-height: 40px; 
            max-height: 120px;
            outline: none;
        }
        .input-box:focus { border-color: var(--accent); }
        .btn { 
            padding: 8px 16px; 
            background: var(--user-bg); 
            color: var(--user-fg);
            border: none; 
            border-radius: 6px; 
            cursor: pointer; 
            font-size: 13px;
        }
        .btn:hover { opacity: 0.9; }
        .btn:disabled { opacity: 0.5; cursor: not-allowed; }
        .btn-secondary { 
            background: transparent; 
            border: 1px solid var(--border);
            color: var(--fg);
        }
        .loading { 
            display: flex; 
            align-items: center; 
            gap: 4px; 
            font-size: 12px;
            color: var(--vscode-descriptionForeground);
            padding: 8px 12px;
        }
        .spinner { 
            width: 14px; 
            height: 14px; 
            border: 2px solid var(--border);
            border-top-color: var(--accent); 
            border-radius: 50%;
            animation: spin 1s linear infinite;
        }
        @keyframes spin { to { transform: rotate(360deg); } }
        .empty-state { 
            text-align: center; 
            color: var(--vscode-descriptionForeground);
            padding: 40px 20px;
        }
        .usage-bar { 
            font-size: 11px; 
            color: var(--vscode-descriptionForeground);
            padding: 4px 12px;
            text-align: right;
        }
    </style>
</head>
<body>
    <div class="header">
        <div class="status-dot" id="statusDot"></div>
        <span id="statusText">Connecting...</span>
        <button class="btn btn-secondary" style="margin-left:auto;padding:4px 8px;font-size:11px;" 
                onclick="checkHealth()">Check</button>
    </div>
    
    <div class="messages" id="messages">
        <div class="empty-state">
            <p>Welcome to Raven</p>
            <p style="font-size:12px;margin-top:8px;">Make sure the server is running: <code>agent serve</code></p>
        </div>
    </div>
    
    <div class="usage-bar" id="usageBar"></div>
    
    <div class="input-area">
        <textarea class="input-box" id="input" placeholder="Ask anything..." 
                  onkeydown="handleKeyDown(event)"></textarea>
        <button class="btn" id="sendBtn" onclick="sendMessage()">Send</button>
        <button class="btn btn-secondary" id="cancelBtn" onclick="cancelRequest()" 
                style="display:none;">Stop</button>
    </div>

    <script>
        const vscode = acquireVsCodeApi();
        let isLoading = false;
        let currentAssistantDiv = null;

        // 初始健康检查
        checkHealth();

        function handleKeyDown(e) {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                sendMessage();
            }
        }

        function sendMessage() {
            const input = document.getElementById('input');
            const text = input.value.trim();
            if (!text || isLoading) return;

            input.value = '';
            addMessage('user', text);
            setLoading(true);
            
            vscode.postMessage({ command: 'send', text });
        }

        function cancelRequest() {
            vscode.postMessage({ command: 'cancel' });
            setLoading(false);
        }

        function checkHealth() {
            vscode.postMessage({ command: 'checkHealth' });
        }

        function addMessage(role, content) {
            const msgs = document.getElementById('messages');
            // 移除 empty state
            if (msgs.querySelector('.empty-state')) {
                msgs.innerHTML = '';
            }

            const div = document.createElement('div');
            div.className = 'message ' + role;
            div.innerHTML = formatContent(content);
            msgs.appendChild(div);
            msgs.scrollTop = msgs.scrollHeight;

            if (role === 'assistant') {
                currentAssistantDiv = div;
            }
            return div;
        }

        function formatContent(text) {
            // 简单的 markdown 格式化
            let html = escapeHtml(text);
            // 代码块
            html = html.replace(/\`\`\`([\s\S]*?)\`\`\`/g, '<pre><code>$1</code></pre>');
            // 行内代码
            html = html.replace(/\`([^\`]+)\`/g, '<code>$1</code>');
            // 粗体
            html = html.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
            // 换行
            html = html.replace(/\n/g, '<br>');
            return html;
        }

        function escapeHtml(text) {
            const div = document.createElement('div');
            div.textContent = text;
            return div.innerHTML;
        }

        function setLoading(loading) {
            isLoading = loading;
            document.getElementById('sendBtn').style.display = loading ? 'none' : '';
            document.getElementById('cancelBtn').style.display = loading ? '' : 'none';
            
            if (loading) {
                currentAssistantDiv = addMessage('assistant', 
                    '<div class="loading"><div class="spinner"></div>Thinking...</div>');
            }
        }

        // 接收来自 extension 的消息
        window.addEventListener('message', (event) => {
            const msg = event.data;
            switch (msg.command) {
                case 'healthStatus':
                    const dot = document.getElementById('statusDot');
                    const text = document.getElementById('statusText');
                    if (msg.connected) {
                        dot.classList.add('connected');
                        text.textContent = 'Connected';
                    } else {
                        dot.classList.remove('connected');
                        text.textContent = 'Disconnected - Run: agent serve';
                    }
                    break;
                case 'streamChunk':
                    if (currentAssistantDiv) {
                        currentAssistantDiv.innerHTML = formatContent(msg.content);
                        document.getElementById('messages').scrollTop = 
                            document.getElementById('messages').scrollHeight;
                    }
                    break;
                case 'toolCall':
                    const toolDiv = document.createElement('div');
                    toolDiv.className = 'tool-call';
                    toolDiv.textContent = '\u{1F527} ' + (msg.name || 'tool');
                    document.getElementById('messages').appendChild(toolDiv);
                    break;
                case 'setLoading':
                    setLoading(msg.loading);
                    if (msg.usage) {
                        document.getElementById('usageBar').textContent = 
                            msg.usage.input + ' in / ' + msg.usage.output + ' out';
                    }
                    break;
                case 'addMessage':
                    addMessage(msg.role, msg.content);
                    break;
            }
        });
    </script>
</body>
</html>`;
    }

    dispose() {
        ChatPanel.currentPanel = undefined;
        this._panel.dispose();
        while (this._disposables.length) {
            this._disposables.pop()?.dispose();
        }
    }
}
