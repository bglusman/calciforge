#!/usr/bin/env python3
"""Mock LLM server for integration testing."""
import json
from flask import Flask, request, jsonify

app = Flask(__name__)
last_request = {}

@app.route('/v1/chat/completions', methods=['POST'])
def chat():
    global last_request
    data = request.get_json()
    last_request = data
    
    # Check if tools were sent (this is what we want to verify)
    has_tools = 'tools' in data
    
    response = {
        'id': 'mock-' + str(hash(str(data))),
        'object': 'chat.completion',
        'model': data.get('model', 'gpt-4'),
        'choices': [{
            'index': 0,
            'message': {
                'role': 'assistant',
                'content': None if has_tools else 'Hello!',
                'tool_calls': [{
                    'id': 'call_' + str(hash(str(data)))[:8],
                    'type': 'function',
                    'function': {
                        'name': 'web_search',
                        'arguments': json.dumps({'query': 'weather'})
                    }
                }] if has_tools else None
            },
            'finish_reason': 'tool_calls' if has_tools else 'stop'
        }]
    }
    return jsonify(response)

@app.route('/v1/models', methods=['GET'])
def models():
    return jsonify({
        'object': 'list',
        'data': [
            {'id': 'gpt-4', 'object': 'model'},
            {'id': 'gpt-3.5-turbo', 'object': 'model'}
        ]
    })

@app.route('/health', methods=['GET'])
def health():
    return jsonify({'status': 'healthy'})

@app.route('/last-request', methods=['GET'])
def get_last_request():
    return jsonify(last_request)

@app.route('/reset', methods=['POST'])
def reset():
    global last_request
    last_request = {}
    return jsonify({'ok': True})

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=8000)
