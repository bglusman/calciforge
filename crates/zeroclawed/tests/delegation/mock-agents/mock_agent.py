#!/usr/bin/env python3
"""
Mock agent for delegation integration testing.

Simulates different agent behaviors:
- planner: Delegates to coder
- coder: Writes code, optionally delegates to reviewer
- reviewer: Reviews code
- echo: Simple echo (for testing connectivity)
"""

import os
import json
from flask import Flask, request, jsonify

app = Flask(__name__)
AGENT_TYPE = os.environ.get('AGENT_TYPE', 'echo')


@app.route('/v1/chat/completions', methods=['POST'])
def chat_completions():
    """OpenAI-compatible chat completions endpoint."""
    data = request.json
    messages = data.get('messages', [])
    last_message = messages[-1]['content'] if messages else ''

    response = generate_response(AGENT_TYPE, last_message)

    return jsonify({
        'id': 'mock-response',
        'object': 'chat.completion',
        'model': f'mock-{AGENT_TYPE}',
        'choices': [{
            'index': 0,
            'message': {
                'role': 'assistant',
                'content': response
            },
            'finish_reason': 'stop'
        }]
    })


@app.route('/health', methods=['GET'])
def health():
    return jsonify({'status': 'healthy', 'agent_type': AGENT_TYPE})


def generate_response(agent_type: str, message: str) -> str:
    """Generate response based on agent type and input."""

    if agent_type == 'echo':
        return f"Echo: {message}"

    elif agent_type == 'planner':
        # Planner analyzes and delegates to coder
        if 'implement' in message.lower() or 'code' in message.lower():
            return f"""Plan for: {message}

I'll break this down and delegate to the coding specialist.

::delegate::{{"target": "coder", "context": "recent", "message": "Implement: {message}"}}::
"""
        return f"Plan: {message}\n\nSteps:\n1. Analyze\n2. Design\n3. Implement"

    elif agent_type == 'coder':
        # Coder writes code, delegates to reviewer if asked
        if 'review' in message.lower() or 'check' in message.lower():
            code = f"""```python
# Implementation for: {message}
def solution():
    # TODO: Implement
    pass
```"""
            return f"""Here's the code:

{code}

::delegate::{{"target": "reviewer", "context": "none", "message": "Review this code:\n{code}"}}::
"""
        return f"""```python
# Implementation for: {message}
def solution():
    # Optimized implementation
    return "result"
```

Code complete!"""

    elif agent_type == 'reviewer':
        # Reviewer provides feedback
        return f"""Code Review:

**Input:** {message[:100]}...

**Feedback:**
- ✅ Structure looks good
- ✅ Follows conventions
- 💡 Consider adding type hints
- 💡 Add unit tests

**Verdict:** Approved with minor suggestions."""

    else:
        return f"Unknown agent type: {agent_type}. Message: {message}"


if __name__ == '__main__':
    port = int(os.environ.get('PORT', 8080))
    print(f"Starting mock agent: {AGENT_TYPE} on port {port}")
    app.run(host='0.0.0.0', port=port)
