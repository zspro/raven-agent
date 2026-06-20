import * as vscode from 'vscode';
import { ChatPanel } from './chatPanel';
import { ApiClient } from './apiClient';

let apiClient: ApiClient;

export function activate(context: vscode.ExtensionContext) {
    const config = vscode.workspace.getConfiguration('agentFramework');
    const baseUrl = config.get<string>('api.baseUrl', 'http://localhost:8080');
    const timeout = config.get<number>('api.timeout', 30000);
    
    apiClient = new ApiClient(baseUrl, timeout);

    // 注册命令
    context.subscriptions.push(
        vscode.commands.registerCommand('agentFramework.openChat', () => {
            ChatPanel.createOrShow(context.extensionUri, apiClient);
        }),
        
        vscode.commands.registerCommand('agentFramework.explainCode', async () => {
            const code = getSelectedCode();
            if (code) {
                const panel = ChatPanel.createOrShow(context.extensionUri, apiClient);
                panel.sendMessage(`Explain this code:\n\n\`\`\`\n${code}\n\`\`\``);
            }
        }),
        
        vscode.commands.registerCommand('agentFramework.fixCode', async () => {
            const code = getSelectedCode();
            if (code) {
                const panel = ChatPanel.createOrShow(context.extensionUri, apiClient);
                panel.sendMessage(`Fix any issues in this code:\n\n\`\`\`\n${code}\n\`\`\``);
            }
        }),
        
        vscode.commands.registerCommand('agentFramework.generateTests', async () => {
            const code = getSelectedCode();
            if (code) {
                const panel = ChatPanel.createOrShow(context.extensionUri, apiClient);
                panel.sendMessage(`Generate unit tests for this code:\n\n\`\`\`\n${code}\n\`\`\``);
            }
        }),
        
        vscode.commands.registerCommand('agentFramework.sendToChat', async () => {
            const code = getSelectedCode();
            if (code) {
                const panel = ChatPanel.createOrShow(context.extensionUri, apiClient);
                panel.sendMessage(`\`\`\`\n${code}\n\`\`\``);
            }
        })
    );

    // 设置上下文
    vscode.commands.executeCommand('setContext', 'agentFramework.enabled', true);
}

export function deactivate() {
    apiClient?.dispose();
}

function getSelectedCode(): string | undefined {
    const editor = vscode.window.activeTextEditor;
    if (!editor) return undefined;
    
    const selection = editor.selection;
    if (selection.isEmpty) {
        // 如果没有选中，获取当前行
        const line = editor.document.lineAt(selection.active.line);
        return line.text;
    }
    
    return editor.document.getText(selection);
}
