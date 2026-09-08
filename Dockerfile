# Placeholder image: static files served by nginx on 8080. Replace this wholesale once the
# app has real code — the only contract k8s/site.yaml relies on is "listens on 8080" and
# "answers /healthz".
FROM nginx:1.27-alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY public/ /usr/share/nginx/html/
EXPOSE 8080
