FROM python:3.11-slim

WORKDIR /app

RUN pip install flask

COPY mock_agent.py .

ENV PORT=8080
ENV AGENT_TYPE=echo

CMD ["python", "mock_agent.py"]
