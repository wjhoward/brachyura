FROM python:alpine3.21
COPY requirements.txt requirements.txt
COPY backend.py backend.py
RUN pip3 install -r requirements.txt
CMD ["python3", "backend.py"]
